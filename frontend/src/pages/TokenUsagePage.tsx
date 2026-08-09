import { useState, useEffect, useCallback } from 'react'
import {
  AreaChart,
  Area,
  BarChart,
  Bar,
  LineChart,
  Line,
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
import { RefreshCw, TrendingUp, Zap, DollarSign, Hash, Layers } from 'lucide-react'
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
  DonutWithLegend,
  StackedComposition,
} from './analyticsShared'
import { LivePricingTable } from './LivePricingTable'
// Cost is formatted by the shared helper so this page and the session list can
// never disagree about what a given figure looks like.
import { formatCost } from '@/lib/format'

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
            width={56}
          />
          <Tooltip
            formatter={(v, name) => [
              formatTokens(Number(v ?? 0)),
              String(name ?? '').replaceAll('_', ' '),
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
    <ChartCard
      title="Cache Hit Rate (%)"
      subtitle="Cache reads as a share of every input-side token — fresh input, cache writes and cache reads. A model with no prompt caching scores 0 rather than being left out of its own denominator."
    >
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
            formatter={v => [`${Number(v ?? 0).toFixed(1)}%`, 'Cache Hit Rate']}
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
            formatter={v => [formatCost(Number(v ?? 0)), 'Estimated Cost']}
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
    { label: 'Input Cost', value: formatCost(cost.input_cost_usd), strong: false },
    { label: 'Output Cost', value: formatCost(cost.output_cost_usd), strong: false },
    { label: 'Cache Read Cost', value: formatCost(cost.cache_read_cost_usd), strong: false },
    { label: 'Cache Write Cost', value: formatCost(cost.cache_write_cost_usd), strong: false },
    // The total shipped in every response but was never rendered, leaving the
    // reader to add four figures to answer the page's headline question.
    { label: 'Total Cost', value: formatCost(cost.total_cost_usd), strong: true },
  ]
  return (
    <div className="grid grid-cols-2 sm:grid-cols-5 gap-3 mb-4">
      {items.map(item => (
        <div
          key={item.label}
          className={`rounded-md border px-3 py-2.5 ${
            item.strong
              ? 'border-emerald-200 dark:border-emerald-800/60 bg-emerald-50 dark:bg-emerald-900/20'
              : 'border-zinc-200 dark:border-zinc-700/50 bg-zinc-50 dark:bg-zinc-800/50'
          }`}
        >
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-1">{item.label}</p>
          <p
            className={`text-base font-semibold ${
              item.strong
                ? 'text-emerald-700 dark:text-emerald-400'
                : 'text-zinc-900 dark:text-zinc-100'
            }`}
          >
            {item.value}
          </p>
        </div>
      ))}
    </div>
  )
}

/**
 * Conversation tokens by model.
 *
 * Deliberately *not* titled as a spend chart. It plots input+output only, and a
 * backend with no prompt caching re-bills its whole context as fresh input
 * every turn while the Anthropic models push theirs through cache read — so on
 * the reference corpus one model held 89.2% of this chart and 13.6% of the
 * money. "Cost by Model" answers the spend question; this one answers where
 * conversation volume went, and says so.
 */
function ModelTokenDonut({ data }: Readonly<{ data: ModelStat[] }>) {
  const slices = data.map(m => ({
    name: formatModelName(m.model),
    value: m.tokens,
    percentage: m.percentage,
  }))
  return (
    <ChartCard
      title="Conversation Tokens by Model"
      subtitle="Input + output only. Cache reads and writes are excluded, so this is not a picture of spend — see Cost by Model for that."
    >
      <DonutWithLegend data={slices} formatValue={formatTokens} />
    </ChartCard>
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
    unique_projects: 0,
    total_tokens: 0,
    total_input_tokens: 0,
    total_output_tokens: 0,
    total_cache_read_tokens: 0,
    total_cache_creation_tokens: 0,
    most_used_model: '',
    avg_tokens_per_session: 0,
    estimated_cost_usd: 0,
    unknown_pricing_tokens: 0,
    unknown_pricing_models: [],
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-zinc-100 dark:border-zinc-700/50 px-4 sm:px-6 py-4 shrink-0">
        <div>
          <h1 className="text-xl font-semibold text-zinc-900 dark:text-zinc-100">Token Usage</h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {summary.total_sessions} session{summary.total_sessions === 1 ? '' : 's'} · {from} →{' '}
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
            {/* KPI Cards.

                "Total Tokens" used to sit beside "Est. Cost" while counting a
                different universe: it excluded cache reads and writes, which
                the cost included and which were most of the money. The tile is
                now named for what it counts, and the two cache tiers each get
                their own tile so nothing large is invisible. */}
            <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6 gap-3">
              <KPICard
                icon={Hash}
                label="Total Sessions"
                value={summary.total_sessions.toLocaleString()}
              />
              <KPICard
                icon={Zap}
                label="Conversation Tokens"
                value={formatTokens(summary.total_tokens)}
                sub={`${formatTokens(summary.total_input_tokens)}↑ ${formatTokens(summary.total_output_tokens)}↓ · excludes cache`}
              />
              <KPICard
                icon={Layers}
                label="Cache Read"
                value={formatTokens(summary.total_cache_read_tokens)}
              />
              <KPICard
                icon={Layers}
                label="Cache Write"
                value={formatTokens(summary.total_cache_creation_tokens)}
              />
              <KPICard
                icon={TrendingUp}
                label="Avg / Session"
                value={formatTokens(Math.round(summary.avg_tokens_per_session))}
                sub="conversation tokens"
              />
              <KPICard
                icon={DollarSign}
                label="Est. Cost"
                value={formatCost(summary.estimated_cost_usd)}
                sub={`all four token types · top model ${formatModelName(summary.most_used_model)}`}
                color="text-emerald-600 dark:text-emerald-400"
              />
            </div>

            {/* The composition the KPI row cannot show: cache traffic dwarfs
                conversation tokens, and the cost tile is priced over all of it. */}
            <ChartCard
              title="Token Composition"
              subtitle="Every token the estimated cost was computed over. Conversation tokens are the two left-hand segments."
            >
              <StackedComposition
                parts={[
                  { label: 'Input', value: summary.total_input_tokens, color: MODEL_COLORS[0] },
                  { label: 'Output', value: summary.total_output_tokens, color: MODEL_COLORS[1] },
                  {
                    label: 'Cache Read',
                    value: summary.total_cache_read_tokens,
                    color: MODEL_COLORS[2],
                    hint: 'billed at ~10% of input',
                  },
                  {
                    label: 'Cache Write',
                    value: summary.total_cache_creation_tokens,
                    color: MODEL_COLORS[3],
                    hint: 'billed above input',
                  },
                ]}
                formatValue={formatTokens}
              />
            </ChartCard>

            {/* Models with no published rates contribute no cost — say so
                rather than letting the estimate look complete. */}
            {summary.unknown_pricing_tokens > 0 && (
              <p className="text-xs text-zinc-500 dark:text-zinc-400">
                Est. cost excludes {formatTokens(summary.unknown_pricing_tokens)} tokens on{' '}
                {summary.unknown_pricing_models.length} model
                {summary.unknown_pricing_models.length === 1 ? '' : 's'} with no published pricing (
                {summary.unknown_pricing_models.join(', ')}).
              </p>
            )}

            {/* Token time series + Cache efficiency */}
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-5">
              <TokenTimeSeriesChart data={report?.time_series ?? []} />
              <CacheEfficiencyChart data={report?.cache_efficiency ?? []} />
            </div>

            {/* Conversation tokens by model */}
            {(report?.model_breakdown?.length ?? 0) > 0 && (
              <ModelTokenDonut data={report!.model_breakdown} />
            )}

            {/* Cost estimation section */}
            <div className="rounded-lg border border-zinc-200 dark:border-zinc-700/50 bg-white dark:bg-zinc-900 p-4">
              <h3 className="text-base font-semibold text-zinc-900 dark:text-zinc-100 mb-3">
                Estimated Cost (USD)
              </h3>
              <LivePricingTable />
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
