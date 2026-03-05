import { useState, useEffect, useCallback } from 'react'
import {
  RadialBarChart,
  RadialBar,
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
  Cell,
  PieChart,
  Pie,
  Legend,
} from 'recharts'
import { insightsApi } from '@/lib/api'
import type { InsightSummary, ToolUsageStat } from '@/types'
import {
  RefreshCw,
  Brain,
  Wrench,
  DollarSign,
  Clock,
  AlertTriangle,
  Layers,
  TrendingUp,
  MessageSquare,
  Zap,
} from 'lucide-react'
import {
  KPICard,
  ChartCard,
  DateRangePicker,
  DatePreset,
  presetToRange,
  subDays,
  fmt,
} from './analyticsShared'

// ─── Formatters ───────────────────────────────────────────────────────────────

function fmtMs(ms: number): string {
  if (ms <= 0) return '0s'
  if (ms < 1000) return `${Math.round(ms)}ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`
  const m = Math.floor(ms / 60_000)
  const s = Math.round((ms % 60_000) / 1000)
  return s > 0 ? `${m}m ${s}s` : `${m}m`
}

function fmtPct(n: number): string {
  return `${(n * 100).toFixed(1)}%`
}

const usdFmt = new Intl.NumberFormat('en-US', {
  style: 'currency',
  currency: 'USD',
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
})

function fmtUsd(n: number): string {
  return usdFmt.format(n)
}

// Tooltip style for light + dark mode
const TOOLTIP_STYLE = {
  fontSize: 12,
  borderRadius: 6,
  backgroundColor: 'var(--color-tooltip-bg, #ffffff)',
  border: '1px solid var(--color-tooltip-border, #e4e4e7)',
  color: 'var(--color-tooltip-text, #18181b)',
}

// ─── Delta badge ──────────────────────────────────────────────────────────────

/**
 * Computes and renders a ±% change badge comparing current vs previous.
 * "higherIsBetter" controls whether a positive delta is green (default) or red (e.g. error rate).
 */
function DeltaBadge({
  current,
  previous,
  higherIsBetter = true,
}: Readonly<{ current: number; previous: number; higherIsBetter?: boolean }>) {
  if (previous === 0) return null
  const delta = ((current - previous) / Math.abs(previous)) * 100
  if (Math.abs(delta) < 0.05) return null // ignore sub-0.05% noise

  const positive = delta > 0
  const good = positive === higherIsBetter
  const sign = positive ? '+' : ''
  const colorClass = good
    ? 'text-emerald-600 dark:text-emerald-400 bg-emerald-50 dark:bg-emerald-900/30'
    : 'text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/30'

  return (
    <span className={`text-[10px] font-medium px-1 py-0.5 rounded ${colorClass}`}>
      {sign}
      {delta.toFixed(1)}%
    </span>
  )
}

// ─── Autonomy Score Gauge ─────────────────────────────────────────────────────

function AutonomyGauge({ score, prevScore }: Readonly<{ score: number; prevScore?: number }>) {
  const clamped = Math.max(0, Math.min(100, score))
  const color = clamped >= 70 ? '#22c55e' : clamped >= 40 ? '#f59e0b' : '#ef4444'
  const label = clamped >= 70 ? 'High' : clamped >= 40 ? 'Medium' : 'Low'

  const data = [
    { value: 100, fill: '#d4d4d8' },
    { name: 'Autonomy', value: clamped, fill: color },
  ]

  return (
    <ChartCard title="Avg. Autonomy Score">
      <div className="flex flex-col items-center justify-center py-2">
        <div className="relative" style={{ width: 180, height: 110 }}>
          <ResponsiveContainer width="100%" height="100%">
            <RadialBarChart
              cx="50%"
              cy="100%"
              innerRadius={60}
              outerRadius={90}
              startAngle={180}
              endAngle={0}
              data={data}
            >
              <RadialBar dataKey="value" cornerRadius={4} background={false} />
            </RadialBarChart>
          </ResponsiveContainer>
          <div className="absolute inset-0 flex flex-col items-center justify-end pb-2">
            <span className="text-3xl font-bold" style={{ color }}>
              {Math.round(clamped)}
            </span>
            {prevScore !== undefined && (
              <DeltaBadge current={score} previous={prevScore} higherIsBetter={true} />
            )}
          </div>
        </div>
        <span className="text-xs text-zinc-500 dark:text-zinc-400 mt-1">{label} autonomy</span>
        <p className="text-xs text-zinc-500 dark:text-zinc-400 text-center mt-2 max-w-xs">
          Measures how independently Claude worked — higher means fewer human interruptions per
          session.
        </p>
      </div>
    </ChartCard>
  )
}

// ─── Top Tools Bar Chart (with optional comparison series) ───────────────────

const TOOL_COLORS = [
  '#6366f1',
  '#22c55e',
  '#f59e0b',
  '#ef4444',
  '#8b5cf6',
  '#14b8a6',
  '#f97316',
  '#ec4899',
  '#06b6d4',
  '#84cc16',
]

function truncateTool(name: string): string {
  return name.length > 22 ? `${name.slice(0, 20)}…` : name
}

interface ToolCompareRow {
  tool: string
  current: number
  previous: number
}

function TopToolsChart({
  tools,
  prevTools,
  hasComparison,
}: Readonly<{ tools: ToolUsageStat[]; prevTools?: ToolUsageStat[]; hasComparison: boolean }>) {
  // Merge current + previous into a single dataset, top 10 by current count
  const top = tools.slice(0, 10)
  const prevMap = new Map((prevTools ?? []).map(t => [t.tool, t.count]))

  const data: ToolCompareRow[] = top.map(t => ({
    tool: t.tool,
    current: t.count,
    previous: prevMap.get(t.tool) ?? 0,
  }))

  return (
    <ChartCard title="Top 10 Tools Used">
      <ResponsiveContainer width="100%" height={300}>
        <BarChart data={data} layout="vertical" margin={{ top: 4, right: 16, left: 8, bottom: 0 }}>
          <CartesianGrid
            strokeDasharray="3 3"
            stroke="#d4d4d8"
            strokeOpacity={0.4}
            horizontal={false}
          />
          <XAxis type="number" tick={{ fontSize: 11 }} tickLine={false} axisLine={false} />
          <YAxis
            type="category"
            dataKey="tool"
            tick={{ fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            width={160}
            tickFormatter={truncateTool}
          />
          <Tooltip
            formatter={(v: number | undefined, name: string | undefined) => [
              (v ?? 0).toLocaleString(),
              name === 'current' ? 'Current period' : 'Previous period',
            ]}
            contentStyle={TOOLTIP_STYLE}
          />
          {hasComparison && (
            <Legend
              wrapperStyle={{ fontSize: 12 }}
              formatter={v => (v === 'current' ? 'Current period' : 'Previous period')}
            />
          )}
          <Bar dataKey="current" name="current" radius={[0, 3, 3, 0]} maxBarSize={16}>
            {data.map((_, i) => (
              <Cell key={`cell-curr-${i}`} fill={TOOL_COLORS[i % TOOL_COLORS.length]} />
            ))}
          </Bar>
          {hasComparison && (
            <Bar
              dataKey="previous"
              name="previous"
              radius={[0, 3, 3, 0]}
              maxBarSize={16}
              fill="#71717a"
              fillOpacity={0.5}
            />
          )}
        </BarChart>
      </ResponsiveContainer>
    </ChartCard>
  )
}

// ─── Cache Efficiency Pie ─────────────────────────────────────────────────────

function CacheEfficiencyPie({
  hitRate,
  prevHitRate,
}: Readonly<{ hitRate: number; prevHitRate?: number }>) {
  const clamped = Math.max(0, Math.min(1, hitRate))
  const data = [
    { name: 'Cache Hit', value: Math.round(clamped * 100), fill: '#22c55e' },
    { name: 'Cache Miss', value: Math.round((1 - clamped) * 100), fill: '#d4d4d8' },
  ]
  return (
    <ChartCard title="Avg. Cache Hit Rate">
      <ResponsiveContainer width="100%" height={220}>
        <PieChart>
          <Pie
            data={data}
            dataKey="value"
            cx="50%"
            cy="50%"
            innerRadius={55}
            outerRadius={80}
            paddingAngle={2}
            label={({ name, value }) => `${name} ${value}%`}
            labelLine={true}
          >
            {data.map((entry, i) => (
              <Cell key={`cell-${i}`} fill={entry.fill} />
            ))}
          </Pie>
          <Legend wrapperStyle={{ fontSize: 12 }} />
          <Tooltip
            formatter={(v: number | undefined) => [`${v ?? 0}%`]}
            contentStyle={TOOLTIP_STYLE}
          />
        </PieChart>
      </ResponsiveContainer>
      {prevHitRate !== undefined && (
        <div className="flex justify-center mt-1">
          <DeltaBadge current={hitRate} previous={prevHitRate} higherIsBetter={true} />
        </div>
      )}
    </ChartCard>
  )
}

// ─── Sessions with Errors ─────────────────────────────────────────────────────

function ErrorSessionsPie({
  withErrors,
  total,
  prevWithErrors,
  prevTotal,
}: Readonly<{
  withErrors: number
  total: number
  prevWithErrors?: number
  prevTotal?: number
}>) {
  const clean = Math.max(0, total - withErrors)
  const data = [
    { name: 'With Errors', value: withErrors, fill: '#ef4444' },
    { name: 'Clean', value: clean, fill: '#22c55e' },
  ]

  const errorRate = total > 0 ? withErrors / total : 0
  const prevErrorRate =
    prevTotal !== undefined && prevWithErrors !== undefined && prevTotal > 0
      ? prevWithErrors / prevTotal
      : undefined

  return (
    <ChartCard title="Session Error Rate">
      <ResponsiveContainer width="100%" height={220}>
        <PieChart>
          <Pie
            data={data}
            dataKey="value"
            cx="50%"
            cy="50%"
            innerRadius={55}
            outerRadius={80}
            paddingAngle={2}
            label={({ name, value }) => `${name}: ${value}`}
            labelLine={true}
          >
            {data.map((entry, i) => (
              <Cell key={`cell-${i}`} fill={entry.fill} />
            ))}
          </Pie>
          <Legend wrapperStyle={{ fontSize: 12 }} />
          <Tooltip
            formatter={(v: number | undefined) => [(v ?? 0).toLocaleString(), 'Sessions']}
            contentStyle={TOOLTIP_STYLE}
          />
        </PieChart>
      </ResponsiveContainer>
      {prevErrorRate !== undefined && (
        <div className="flex justify-center mt-1">
          <DeltaBadge current={errorRate} previous={prevErrorRate} higherIsBetter={false} />
        </div>
      )}
    </ChartCard>
  )
}

// ─── Productivity Score Card ──────────────────────────────────────────────────

function productivityScore(s: InsightSummary): number {
  const errorFreeRatio =
    s.total_sessions > 0 ? (s.total_sessions - s.sessions_with_errors) / s.total_sessions : 1
  return Math.min(
    100,
    Math.max(
      0,
      Math.round(
        s.avg_autonomy_score * 0.5 + s.avg_cache_hit_rate * 100 * 0.3 + errorFreeRatio * 100 * 0.2,
      ),
    ),
  )
}

function ProductivityCard({
  summary,
  prev,
}: Readonly<{ summary: InsightSummary; prev?: InsightSummary }>) {
  const errorFreeRatio =
    summary.total_sessions > 0
      ? (summary.total_sessions - summary.sessions_with_errors) / summary.total_sessions
      : 1
  const score = productivityScore(summary)
  const prevScore = prev ? productivityScore(prev) : undefined
  const color =
    score >= 70
      ? 'text-emerald-600 dark:text-emerald-400'
      : score >= 45
        ? 'text-amber-600 dark:text-amber-400'
        : 'text-red-600 dark:text-red-400'
  const tier = score >= 70 ? 'Efficient' : score >= 45 ? 'Moderate' : 'Needs attention'

  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700/50 bg-white dark:bg-zinc-900 p-5 flex flex-col gap-1">
      <p className="text-xs font-semibold uppercase tracking-widest text-zinc-400 dark:text-zinc-500">
        Overall Productivity Score
      </p>
      <div className="flex items-end gap-3 mt-1">
        <span className={`text-5xl font-bold ${color}`}>{score}</span>
        <div className="flex flex-col mb-1.5 gap-1">
          <span className="text-sm text-zinc-500 dark:text-zinc-400">/ 100 · {tier}</span>
          {prevScore !== undefined && (
            <DeltaBadge current={score} previous={prevScore} higherIsBetter={true} />
          )}
        </div>
      </div>
      <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-1">
        Composite of autonomy (50%), cache efficiency (30%), and error-free sessions (20%).
      </p>
      <div className="mt-3 grid grid-cols-3 gap-2 text-center">
        {[
          {
            label: 'Autonomy',
            value: `${Math.round(summary.avg_autonomy_score)}`,
            sub: '/ 100',
            w: '50%',
          },
          {
            label: 'Cache Hit',
            value: fmtPct(summary.avg_cache_hit_rate),
            sub: 'avg',
            w: '30%',
          },
          {
            label: 'Clean Sessions',
            value: fmtPct(errorFreeRatio),
            sub: 'no errors',
            w: '20%',
          },
        ].map(item => (
          <div key={item.label} className="rounded-md bg-zinc-50 dark:bg-zinc-800/60 px-2 py-1.5">
            <p className="text-base font-semibold text-zinc-900 dark:text-zinc-100">{item.value}</p>
            <p className="text-[10px] text-zinc-500 dark:text-zinc-400">{item.label}</p>
            <p className="text-[9px] text-zinc-400 dark:text-zinc-600">weight {item.w}</p>
          </div>
        ))}
      </div>
    </div>
  )
}

// ─── KPI row with optional delta ──────────────────────────────────────────────

interface KPIWithDeltaProps {
  icon: React.ComponentType<{ className?: string }>
  label: string
  value: string
  color?: string
  current: number
  previous?: number
  higherIsBetter?: boolean
}

function KPIWithDelta({
  icon,
  label,
  value,
  color,
  current,
  previous,
  higherIsBetter = true,
}: Readonly<KPIWithDeltaProps>) {
  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700/50 bg-white dark:bg-zinc-900 px-3 py-2.5 flex flex-col gap-1">
      <div className="flex items-center justify-between gap-1">
        <KPICard icon={icon} label={label} value={value} color={color} />
      </div>
      {previous !== undefined && previous !== 0 && (
        <div className="flex justify-start pl-1">
          <DeltaBadge current={current} previous={previous} higherIsBetter={higherIsBetter} />
        </div>
      )}
    </div>
  )
}

// ─── Previous period date calculation ────────────────────────────────────────

/** Calculates the previous period of the same length as [from, to]. */
function previousPeriod(from: string, to: string): { from: string; to: string } {
  const f = new Date(from + 'T00:00:00')
  const t = new Date(to + 'T00:00:00')
  const diffDays = Math.round((t.getTime() - f.getTime()) / 86_400_000)
  const prevTo = subDays(f, 1)
  const prevFrom = subDays(f, diffDays + 1)
  return { from: fmt(prevFrom), to: fmt(prevTo) }
}

// ─── Populated content (extracted to keep InsightsPage complexity low) ────────

interface InsightsContentProps {
  summary: InsightSummary
  prevSummary: InsightSummary | null
  hasComparison: boolean
  from: string
  to: string
}

function InsightsContent({
  summary,
  prevSummary,
  hasComparison,
  from,
  to,
}: Readonly<InsightsContentProps>) {
  // Use optional chaining via a nullable reference — avoids repeated ternaries.
  const prev = hasComparison ? prevSummary : null

  const errorFreeRate =
    summary.total_sessions > 0
      ? (summary.total_sessions - summary.sessions_with_errors) / summary.total_sessions
      : 1

  const prevErrorFreeRate =
    prev && prev.total_sessions > 0
      ? (prev.total_sessions - prev.sessions_with_errors) / prev.total_sessions
      : undefined

  const periodDays =
    Math.round((new Date(to).getTime() - new Date(from).getTime()) / 86_400_000) + 1

  return (
    <>
      {/* Productivity Score */}
      <ProductivityCard summary={summary} prev={prev ?? undefined} />

      {/* KPI Cards row 1 */}
      <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-5 gap-3">
        <KPICard
          icon={MessageSquare}
          label="Total Sessions"
          value={summary.total_sessions.toLocaleString()}
        />
        <KPIWithDelta
          icon={Brain}
          label="Avg Autonomy"
          value={`${Math.round(summary.avg_autonomy_score)} / 100`}
          current={summary.avg_autonomy_score}
          previous={prev?.avg_autonomy_score}
          higherIsBetter={true}
        />
        <KPIWithDelta
          icon={TrendingUp}
          label="Avg Turns"
          value={summary.avg_turn_count.toFixed(1)}
          current={summary.avg_turn_count}
          previous={prev?.avg_turn_count}
          higherIsBetter={true}
        />
        <KPIWithDelta
          icon={Wrench}
          label="Avg Tool Calls"
          value={Math.round(summary.avg_tool_calls_total).toLocaleString()}
          current={summary.avg_tool_calls_total}
          previous={prev?.avg_tool_calls_total}
          higherIsBetter={true}
        />
        <KPIWithDelta
          icon={Zap}
          label="Avg Cache Hit"
          value={fmtPct(summary.avg_cache_hit_rate)}
          color="text-amber-600 dark:text-amber-400"
          current={summary.avg_cache_hit_rate}
          previous={prev?.avg_cache_hit_rate}
          higherIsBetter={true}
        />
      </div>

      {/* KPI Cards row 2 */}
      <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-5 gap-3">
        <KPIWithDelta
          icon={Clock}
          label="Avg Duration"
          value={fmtMs(summary.avg_total_duration_ms)}
          current={summary.avg_total_duration_ms}
          previous={prev?.avg_total_duration_ms}
          higherIsBetter={false}
        />
        <KPIWithDelta
          icon={DollarSign}
          label="Avg Cost"
          value={fmtUsd(summary.avg_cost_estimate_usd)}
          color="text-emerald-600 dark:text-emerald-400"
          current={summary.avg_cost_estimate_usd}
          previous={prev?.avg_cost_estimate_usd}
          higherIsBetter={false}
        />
        <KPICard
          icon={DollarSign}
          label="Total Cost"
          value={fmtUsd(summary.total_cost_estimate_usd)}
          color="text-emerald-600 dark:text-emerald-400"
        />
        <KPIWithDelta
          icon={AlertTriangle}
          label="Sessions w/ Errors"
          value={summary.sessions_with_errors.toLocaleString()}
          color={
            summary.sessions_with_errors > 0
              ? 'text-red-600 dark:text-red-400'
              : 'text-emerald-600 dark:text-emerald-400'
          }
          current={summary.sessions_with_errors}
          previous={prev?.sessions_with_errors}
          higherIsBetter={false}
        />
        <KPIWithDelta
          icon={Layers}
          label="Error-Free Rate"
          value={fmtPct(errorFreeRate)}
          color="text-emerald-600 dark:text-emerald-400"
          current={errorFreeRate}
          previous={prevErrorFreeRate}
          higherIsBetter={true}
        />
      </div>

      {/* Autonomy Gauge + Cache Pie + Error Pie */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-5">
        <AutonomyGauge score={summary.avg_autonomy_score} prevScore={prev?.avg_autonomy_score} />
        <CacheEfficiencyPie
          hitRate={summary.avg_cache_hit_rate}
          prevHitRate={prev?.avg_cache_hit_rate}
        />
        <ErrorSessionsPie
          withErrors={summary.sessions_with_errors}
          total={summary.total_sessions}
          prevWithErrors={prev?.sessions_with_errors}
          prevTotal={prev?.total_sessions}
        />
      </div>

      {/* Top Tools */}
      {summary.top_tools.length > 0 && (
        <TopToolsChart
          tools={summary.top_tools}
          prevTools={prev?.top_tools ?? []}
          hasComparison={hasComparison}
        />
      )}

      {/* Footer note */}
      <p className="text-xs text-zinc-400 dark:text-zinc-500 text-center pb-2">
        Insights are computed from Claude Code session JSONL files and updated incrementally in the
        background. Cost estimates use approximate pricing and may not reflect current Anthropic
        rates.{' '}
        {hasComparison &&
          `Δ badges compare the selected period against the equally-sized preceding ${periodDays} days.`}
      </p>
    </>
  )
}

// ─── Main Page ────────────────────────────────────────────────────────────────

export default function InsightsPage() {
  const [summary, setSummary] = useState<InsightSummary | null>(null)
  const [prevSummary, setPrevSummary] = useState<InsightSummary | null>(null)
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [preset, setPreset] = useState<DatePreset>('30d')
  const [from, setFrom] = useState(() => presetToRange('30d').from)
  const [to, setTo] = useState(() => presetToRange('30d').to)

  const isAllTime = preset === 'all-time'

  const load = useCallback(async (f: string, t: string, allTime: boolean) => {
    try {
      const [curr, prev] = await Promise.all([
        insightsApi.getSummary({ from: allTime ? undefined : f, to: allTime ? undefined : t }),
        allTime ? Promise.resolve(null) : insightsApi.getSummary(previousPeriod(f, t)),
      ])
      setSummary(curr)
      setPrevSummary(prev)
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load insights')
    } finally {
      setLoading(false)
      setRefreshing(false)
    }
  }, [])

  useEffect(() => {
    void load(from, to, isAllTime)
  }, [load, from, to, isAllTime])

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
    void load(from, to, isAllTime)
  }

  const hasComparison = !isAllTime && prevSummary !== null && prevSummary.total_sessions > 0

  const subtitle =
    summary && summary.total_sessions > 0
      ? `${summary.total_sessions.toLocaleString()} session${summary.total_sessions === 1 ? '' : 's'} analysed`
      : 'Productivity & efficiency metrics for your Claude Code sessions'

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-zinc-100 dark:border-zinc-700/50 px-4 sm:px-6 py-4 shrink-0">
        <div>
          <div className="flex items-center gap-2">
            <h1 className="text-xl font-semibold text-zinc-900 dark:text-zinc-100">Insights</h1>
            <span className="text-[10px] font-semibold uppercase tracking-wide px-1.5 py-0.5 rounded bg-amber-100 dark:bg-amber-900/40 text-amber-700 dark:text-amber-400">
              Experimental
            </span>
          </div>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">{subtitle}</p>
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

      {/* Date range controls */}
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
        />
      </div>

      {/* Experimental notice */}
      <div className="px-4 sm:px-6 py-2.5 border-b border-amber-200 dark:border-amber-800/50 bg-amber-50 dark:bg-amber-950/30 shrink-0">
        <p className="text-xs text-amber-800 dark:text-amber-300">
          <span className="font-semibold">Experimental:</span> These metrics are based on heuristics
          and may not fully reflect your actual productivity. Formulas and weights will be refined
          in upcoming releases.{' '}
          <a
            href="https://github.com/shaharia-lab/agento/discussions/110"
            target="_blank"
            rel="noreferrer"
            className="underline underline-offset-2 hover:text-amber-900 dark:hover:text-amber-200"
          >
            Share your feedback →
          </a>
        </p>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto px-4 sm:px-6 py-5 space-y-5">
        {error && (
          <div className="rounded-md border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-950 px-4 py-2.5 text-sm text-red-700 dark:text-red-300">
            {error}
          </div>
        )}

        {loading ? (
          <div className="flex items-center justify-center py-20">
            <p className="text-sm text-zinc-400">Analysing sessions…</p>
          </div>
        ) : summary && summary.total_sessions > 0 ? (
          <InsightsContent
            summary={summary}
            prevSummary={prevSummary}
            hasComparison={hasComparison}
            from={from}
            to={to}
          />
        ) : (
          <div className="flex flex-col items-center justify-center py-20 gap-2">
            <p className="text-sm text-zinc-500 dark:text-zinc-400">
              No sessions found for this period.
            </p>
            <p className="text-xs text-zinc-400 dark:text-zinc-500 text-center max-w-sm">
              Try a wider date range, or wait for Claude Code sessions to be scanned and processed
              in the background.
            </p>
          </div>
        )}
      </div>
    </div>
  )
}
