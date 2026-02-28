import { useState, useEffect, useCallback } from 'react'
import {
  AreaChart,
  Area,
  BarChart,
  Bar,
  LineChart,
  Line,
  PieChart,
  Pie,
  Cell,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
  ResponsiveContainer,
} from 'recharts'
import { analyticsApi } from '@/lib/api'
import type {
  AnalyticsReport,
  AnalyticsSummary,
  TimeSeriesPoint,
  CacheEfficiencyPoint,
  ModelStat,
  CostPoint,
  CostSummary,
} from '@/types'
import {
  RefreshCw,
  TrendingUp,
  Zap,
  DollarSign,
  Hash,
  Clock,
  Layers,
  ChevronDown,
} from 'lucide-react'
import {
  MODEL_COLORS,
  DatePreset,
  presetToRange,
  formatTokens,
  formatModelName,
  formatDateLabel,
  KPICard,
  ChartCard,
  DateRangePicker,
} from './analyticsShared'

// ─── USD formatting ────────────────────────────────────────────────────────────

const usdFmt2 = new Intl.NumberFormat('en-US', {
  style: 'currency',
  currency: 'USD',
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
})
const usdFmt4 = new Intl.NumberFormat('en-US', {
  style: 'currency',
  currency: 'USD',
  minimumFractionDigits: 4,
  maximumFractionDigits: 4,
})

function formatCost(n: number): string {
  if (n < 0.0001) return usdFmt2.format(0) // $0.00
  if (n < 0.01) return `< ${usdFmt2.format(0.01)}` // < $0.01
  if (n < 1) return usdFmt4.format(n) // $0.0123 — extra precision for sub-dollar amounts
  return usdFmt2.format(n) // $1.23 / $1,234.56
}

// ─── Charts ───────────────────────────────────────────────────────────────────

function TokenTimeSeriesChart({ data }: Readonly<{ data: TimeSeriesPoint[] }>) {
  const formatted = data.map(d => ({ ...d, date: formatDateLabel(d.date) }))
  return (
    <ChartCard title="Token Usage Over Time">
      <ResponsiveContainer width="100%" height={280}>
        <AreaChart data={formatted} margin={{ top: 4, right: 8, left: 0, bottom: 0 }}>
          <CartesianGrid strokeDasharray="3 3" stroke="#27272a" strokeOpacity={0.5} />
          <XAxis
            dataKey="date"
            tick={{ fontSize: 11 }}
            tickLine={false}
            interval="preserveStartEnd"
          />
          <YAxis
            tickFormatter={formatTokens}
            tick={{ fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            width={48}
          />
          <Tooltip
            formatter={(v: number | undefined, name: string | undefined) => [
              formatTokens(v ?? 0),
              (name ?? '').replace(/_/g, ' '),
            ]}
            contentStyle={{ fontSize: 12, borderRadius: 6 }}
          />
          <Legend wrapperStyle={{ fontSize: 12 }} />
          <Area
            type="monotone"
            dataKey="input_tokens"
            name="Input"
            stroke="#6366f1"
            fill="#6366f1"
            fillOpacity={0.15}
            strokeWidth={1.5}
          />
          <Area
            type="monotone"
            dataKey="output_tokens"
            name="Output"
            stroke="#22c55e"
            fill="#22c55e"
            fillOpacity={0.15}
            strokeWidth={1.5}
          />
          <Area
            type="monotone"
            dataKey="cache_read_tokens"
            name="Cache Read"
            stroke="#f59e0b"
            fill="#f59e0b"
            fillOpacity={0.15}
            strokeWidth={1.5}
          />
        </AreaChart>
      </ResponsiveContainer>
    </ChartCard>
  )
}

function CacheEfficiencyChart({ data }: Readonly<{ data: CacheEfficiencyPoint[] }>) {
  const formatted = data.map(d => ({ ...d, date: formatDateLabel(d.date) }))
  return (
    <ChartCard title="Cache Hit Rate (%)">
      <ResponsiveContainer width="100%" height={280}>
        <LineChart data={formatted} margin={{ top: 4, right: 8, left: 0, bottom: 0 }}>
          <CartesianGrid strokeDasharray="3 3" stroke="#27272a" strokeOpacity={0.5} />
          <XAxis
            dataKey="date"
            tick={{ fontSize: 11 }}
            tickLine={false}
            interval="preserveStartEnd"
          />
          <YAxis
            domain={[0, 100]}
            tickFormatter={v => `${v}%`}
            tick={{ fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            width={40}
          />
          <Tooltip
            formatter={(v: number | undefined) => [`${(v ?? 0).toFixed(1)}%`, 'Cache Hit Rate']}
            contentStyle={{ fontSize: 12, borderRadius: 6 }}
          />
          <Line
            type="monotone"
            dataKey="cache_hit_rate"
            name="Cache Hit Rate"
            stroke="#f59e0b"
            strokeWidth={2}
            dot={false}
          />
        </LineChart>
      </ResponsiveContainer>
    </ChartCard>
  )
}

function CostOverTimeChart({ data }: Readonly<{ data: CostPoint[] }>) {
  const formatted = data.map(d => ({ ...d, date: formatDateLabel(d.date) }))
  return (
    <ChartCard title="Estimated Cost Over Time (USD)">
      <ResponsiveContainer width="100%" height={240}>
        <BarChart data={formatted} margin={{ top: 4, right: 8, left: 0, bottom: 0 }}>
          <CartesianGrid strokeDasharray="3 3" stroke="#27272a" strokeOpacity={0.5} />
          <XAxis
            dataKey="date"
            tick={{ fontSize: 11 }}
            tickLine={false}
            interval="preserveStartEnd"
          />
          <YAxis
            tickFormatter={v => formatCost(v as number)}
            tick={{ fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            width={56}
          />
          <Tooltip
            formatter={(v: number | undefined) => [formatCost(v ?? 0), 'Estimated Cost']}
            contentStyle={{ fontSize: 12, borderRadius: 6 }}
          />
          <Bar dataKey="estimated_cost_usd" name="Cost" fill="#6366f1" radius={[2, 2, 0, 0]} />
        </BarChart>
      </ResponsiveContainer>
    </ChartCard>
  )
}

function CostSummaryCards({ cost }: Readonly<{ cost: CostSummary }>) {
  const items = [
    { label: 'Input Cost', value: formatCost(cost.input_cost_usd) },
    { label: 'Output Cost', value: formatCost(cost.output_cost_usd) },
    { label: 'Cache Read Cost', value: formatCost(cost.cache_read_cost_usd) },
    { label: 'Cache Write Cost', value: formatCost(cost.cache_write_cost_usd) },
  ]
  return (
    <div className="grid grid-cols-2 sm:grid-cols-4 gap-3 mb-4">
      {items.map(item => (
        <div
          key={item.label}
          className="rounded-md border border-zinc-200 dark:border-zinc-700/50 bg-zinc-50 dark:bg-zinc-800/50 px-3 py-2.5"
        >
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-1">{item.label}</p>
          <p className="text-base font-semibold text-zinc-900 dark:text-zinc-100">{item.value}</p>
        </div>
      ))}
    </div>
  )
}

function ModelPieChart({ data }: Readonly<{ data: ModelStat[] }>) {
  return (
    <ChartCard title="Token Distribution by Model">
      <ResponsiveContainer width="100%" height={280}>
        <PieChart>
          <Pie
            data={data}
            dataKey="tokens"
            nameKey="model"
            cx="50%"
            cy="45%"
            outerRadius={90}
            label={({ name, percent }) =>
              `${formatModelName(name as string)} ${((percent as number) * 100).toFixed(1)}%`
            }
            labelLine={true}
          >
            {data.map((_, i) => (
              <Cell key={i} fill={MODEL_COLORS[i % MODEL_COLORS.length]} />
            ))}
          </Pie>
          <Tooltip
            formatter={(v: number | undefined, name: string | undefined) => [
              formatTokens(v ?? 0),
              formatModelName(name ?? ''),
            ]}
            contentStyle={{ fontSize: 12, borderRadius: 6 }}
          />
        </PieChart>
      </ResponsiveContainer>
    </ChartCard>
  )
}

// ─── Pricing Disclaimer ───────────────────────────────────────────────────────

function PricingDisclaimer() {
  const [open, setOpen] = useState(false)
  return (
    <div className="rounded-md border border-amber-200 dark:border-amber-800/50 bg-amber-50 dark:bg-amber-900/20 mb-4">
      <button
        onClick={() => setOpen(o => !o)}
        className="flex w-full items-center justify-between px-3 py-2 text-xs font-medium text-amber-800 dark:text-amber-300 cursor-pointer"
      >
        <span>⚠ Estimates only — these figures may be outdated</span>
        <ChevronDown
          className={`h-3.5 w-3.5 shrink-0 transition-transform duration-200 ${open ? 'rotate-180' : ''}`}
        />
      </button>
      {open && (
        <div className="px-3 pb-2.5 border-t border-amber-200 dark:border-amber-700/50 pt-2">
          <p className="text-xs text-amber-700 dark:text-amber-400 mb-2">
            Rates below were sourced from the{' '}
            <a
              href="https://platform.claude.com/docs/en/about-claude/pricing"
              target="_blank"
              rel="noopener noreferrer"
              className="underline hover:text-amber-900 dark:hover:text-amber-200"
            >
              Anthropic API pricing page
            </a>{' '}
            in February 2026. Anthropic updates pricing periodically — always verify against the{' '}
            <a
              href="https://platform.claude.com/docs/en/about-claude/pricing"
              target="_blank"
              rel="noopener noreferrer"
              className="underline hover:text-amber-900 dark:hover:text-amber-200"
            >
              official pricing page
            </a>{' '}
            before relying on these numbers. Model tier is detected from the model name; unknown
            models fall back to Sonnet pricing.
          </p>
          <table className="w-full text-[11px] text-amber-800 dark:text-amber-300 border-collapse">
            <thead>
              <tr className="border-b border-amber-200 dark:border-amber-700/50">
                <th className="text-left font-medium pb-1 pr-4">Model</th>
                <th className="text-right font-medium pb-1 pr-4">Input</th>
                <th className="text-right font-medium pb-1 pr-4">Output</th>
                <th className="text-right font-medium pb-1 pr-4">Cache write</th>
                <th className="text-right font-medium pb-1">Cache read</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-amber-100 dark:divide-amber-800/30">
              {[
                {
                  model: 'Claude Opus 4.5/4.6',
                  input: '$5',
                  output: '$25',
                  cacheWrite: '$6.25',
                  cacheRead: '$0.50',
                },
                {
                  model: 'Claude Sonnet 4.x',
                  input: '$3',
                  output: '$15',
                  cacheWrite: '$3.75',
                  cacheRead: '$0.30',
                },
                {
                  model: 'Claude Haiku 4.5',
                  input: '$1',
                  output: '$5',
                  cacheWrite: '$1.25',
                  cacheRead: '$0.10',
                },
              ].map(row => (
                <tr key={row.model}>
                  <td className="py-0.5 pr-4 font-medium">{row.model}</td>
                  <td className="py-0.5 pr-4 text-right">{row.input}</td>
                  <td className="py-0.5 pr-4 text-right">{row.output}</td>
                  <td className="py-0.5 pr-4 text-right">{row.cacheWrite}</td>
                  <td className="py-0.5 text-right">{row.cacheRead}</td>
                </tr>
              ))}
            </tbody>
            <tfoot>
              <tr>
                <td colSpan={5} className="pt-1.5 text-amber-600 dark:text-amber-500 italic">
                  All rates are per million tokens (MTok).
                </td>
              </tr>
            </tfoot>
          </table>
        </div>
      )}
    </div>
  )
}

// ─── Main Page ────────────────────────────────────────────────────────────────

export default function TokenUsagePage() {
  const [report, setReport] = useState<AnalyticsReport | null>(null)
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [preset, setPreset] = useState<DatePreset>('30d')
  const [from, setFrom] = useState(() => presetToRange('30d').from)
  const [to, setTo] = useState(() => presetToRange('30d').to)
  const [project, setProject] = useState('all')

  const load = useCallback(async (f: string, t: string, proj: string) => {
    try {
      const data = await analyticsApi.get({
        from: f,
        to: t,
        project: proj === 'all' ? undefined : proj,
      })
      setReport(data)
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load analytics')
    } finally {
      setLoading(false)
      setRefreshing(false)
    }
  }, [])

  useEffect(() => {
    load(from, to, project)
  }, [load, from, to, project])

  const handlePreset = (p: DatePreset) => {
    setPreset(p)
    if (p !== 'custom') {
      const range = presetToRange(p)
      setFrom(range.from)
      setTo(range.to)
    }
  }

  const handleRefresh = () => {
    setRefreshing(true)
    load(from, to, project)
  }

  const summary: AnalyticsSummary = report?.summary ?? {
    total_sessions: 0,
    total_tokens: 0,
    total_input_tokens: 0,
    total_output_tokens: 0,
    total_cache_read_tokens: 0,
    total_cache_creation_tokens: 0,
    most_used_model: '',
    avg_tokens_per_session: 0,
    estimated_cost_usd: 0,
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-zinc-100 dark:border-zinc-700/50 px-4 sm:px-6 py-4 shrink-0">
        <div>
          <h1 className="text-base font-semibold text-zinc-900 dark:text-zinc-100">Token Usage</h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {summary.total_sessions} session{summary.total_sessions !== 1 ? 's' : ''} · {from} →{' '}
            {to}
          </p>
        </div>
        <button
          onClick={handleRefresh}
          disabled={refreshing || loading}
          className="flex items-center gap-1.5 rounded-md border border-zinc-200 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-1.5 text-xs text-zinc-600 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 disabled:opacity-50 transition-colors"
        >
          <RefreshCw className={`h-3.5 w-3.5 ${refreshing ? 'animate-spin' : ''}`} />
          Refresh
        </button>
      </div>

      {/* Controls */}
      <div className="px-4 sm:px-6 py-3 border-b border-zinc-100 dark:border-zinc-700/50 shrink-0">
        <DateRangePicker
          preset={preset}
          from={from}
          to={to}
          onPreset={handlePreset}
          onFrom={v => {
            setFrom(v)
            setPreset('custom')
          }}
          onTo={v => {
            setTo(v)
            setPreset('custom')
          }}
          projects={report?.projects}
          project={project}
          onProject={setProject}
        />
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto px-4 sm:px-6 py-5 space-y-5">
        {error && (
          <div className="rounded-md border border-red-200 bg-red-50 px-4 py-2.5 text-sm text-red-700">
            {error}
          </div>
        )}

        {loading ? (
          <div className="flex items-center justify-center py-20">
            <p className="text-sm text-zinc-400">Loading analytics…</p>
          </div>
        ) : (
          <>
            {/* KPI Cards */}
            <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6 gap-3">
              <KPICard
                icon={Hash}
                label="Total Sessions"
                value={summary.total_sessions.toLocaleString()}
              />
              <KPICard
                icon={Zap}
                label="Total Tokens"
                value={formatTokens(summary.total_tokens)}
                sub={`${formatTokens(summary.total_input_tokens)}↑ ${formatTokens(summary.total_output_tokens)}↓`}
              />
              <KPICard
                icon={Layers}
                label="Cache Read"
                value={formatTokens(summary.total_cache_read_tokens)}
              />
              <KPICard
                icon={TrendingUp}
                label="Avg / Session"
                value={formatTokens(Math.round(summary.avg_tokens_per_session))}
              />
              <KPICard
                icon={Clock}
                label="Top Model"
                value={formatModelName(summary.most_used_model)}
              />
              <KPICard
                icon={DollarSign}
                label="Est. Cost"
                value={formatCost(summary.estimated_cost_usd)}
                color="text-emerald-600 dark:text-emerald-400"
              />
            </div>

            {/* Token time series + Cache efficiency */}
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-5">
              <TokenTimeSeriesChart data={report?.time_series ?? []} />
              <CacheEfficiencyChart data={report?.cache_efficiency ?? []} />
            </div>

            {/* Token distribution by model */}
            {(report?.model_breakdown?.length ?? 0) > 0 && (
              <ModelPieChart data={report!.model_breakdown} />
            )}

            {/* Cost estimation section */}
            <div className="rounded-lg border border-zinc-200 dark:border-zinc-700/50 bg-white dark:bg-zinc-900 p-4">
              <h3 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100 mb-3">
                Estimated Cost (USD)
              </h3>
              <PricingDisclaimer />
              <CostSummaryCards
                cost={
                  report?.cost_summary ?? {
                    input_cost_usd: 0,
                    output_cost_usd: 0,
                    cache_read_cost_usd: 0,
                    cache_write_cost_usd: 0,
                    total_cost_usd: 0,
                  }
                }
              />
              <CostOverTimeChart data={report?.cost_over_time ?? []} />
            </div>
          </>
        )}
      </div>
    </div>
  )
}
