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
import { KPICard, ChartCard } from './analyticsShared'

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

// Tooltip style consistent with light + dark mode
const TOOLTIP_STYLE = {
  fontSize: 12,
  borderRadius: 6,
  backgroundColor: 'var(--color-tooltip-bg, #ffffff)',
  border: '1px solid var(--color-tooltip-border, #e4e4e7)',
  color: 'var(--color-tooltip-text, #18181b)',
}

// ─── Autonomy Score Gauge ─────────────────────────────────────────────────────

function AutonomyGauge({ score }: Readonly<{ score: number }>) {
  const clamped = Math.max(0, Math.min(100, score))
  const color = clamped >= 70 ? '#22c55e' : clamped >= 40 ? '#f59e0b' : '#ef4444'
  const label = clamped >= 70 ? 'High' : clamped >= 40 ? 'Medium' : 'Low'

  // Use a neutral that reads well in both light (#d4d4d8 = zinc-300) and dark mode
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
            <span className="text-xs text-zinc-500 dark:text-zinc-400">{label} autonomy</span>
          </div>
        </div>
        <p className="text-xs text-zinc-500 dark:text-zinc-400 text-center mt-2 max-w-xs">
          Measures how independently Claude worked — higher means fewer human interruptions per
          session.
        </p>
      </div>
    </ChartCard>
  )
}

// ─── Top Tools Bar Chart ──────────────────────────────────────────────────────

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

function TopToolsChart({ tools }: Readonly<{ tools: ToolUsageStat[] }>) {
  const top = tools.slice(0, 10)
  return (
    <ChartCard title="Top 10 Tools Used">
      <ResponsiveContainer width="100%" height={280}>
        <BarChart data={top} layout="vertical" margin={{ top: 4, right: 16, left: 8, bottom: 0 }}>
          <CartesianGrid
            strokeDasharray="3 3"
            stroke="#27272a"
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
            formatter={(v: number | undefined) => [(v ?? 0).toLocaleString(), 'Calls']}
            contentStyle={TOOLTIP_STYLE}
          />
          <Bar dataKey="count" radius={[0, 3, 3, 0]}>
            {top.map((_, i) => (
              <Cell key={`cell-${i}`} fill={TOOL_COLORS[i % TOOL_COLORS.length]} />
            ))}
          </Bar>
        </BarChart>
      </ResponsiveContainer>
    </ChartCard>
  )
}

// ─── Cache Efficiency Pie ─────────────────────────────────────────────────────

function CacheEfficiencyPie({ hitRate }: Readonly<{ hitRate: number }>) {
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
    </ChartCard>
  )
}

// ─── Sessions with Errors ─────────────────────────────────────────────────────

function ErrorSessionsPie({ withErrors, total }: Readonly<{ withErrors: number; total: number }>) {
  const clean = Math.max(0, total - withErrors)
  const data = [
    { name: 'With Errors', value: withErrors, fill: '#ef4444' },
    { name: 'Clean', value: clean, fill: '#22c55e' },
  ]
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
    </ChartCard>
  )
}

// ─── Productivity Score Card ──────────────────────────────────────────────────

function ProductivityCard({ summary }: Readonly<{ summary: InsightSummary }>) {
  // Composite productivity score: weighted blend of autonomy, cache hit rate, and error-free ratio
  const errorFreeRatio =
    summary.total_sessions > 0
      ? (summary.total_sessions - summary.sessions_with_errors) / summary.total_sessions
      : 1
  const score = Math.min(
    100,
    Math.max(
      0,
      Math.round(
        summary.avg_autonomy_score * 0.5 +
          summary.avg_cache_hit_rate * 100 * 0.3 +
          errorFreeRatio * 100 * 0.2,
      ),
    ),
  )
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
        <span className="text-sm text-zinc-500 dark:text-zinc-400 mb-1.5">/ 100 · {tier}</span>
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

// ─── Main Page ────────────────────────────────────────────────────────────────

export default function InsightsPage() {
  const [summary, setSummary] = useState<InsightSummary | null>(null)
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      const data = await insightsApi.getSummary()
      setSummary(data)
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load insights')
    } finally {
      setLoading(false)
      setRefreshing(false)
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  const handleRefresh = () => {
    setRefreshing(true)
    void load()
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-zinc-100 dark:border-zinc-700/50 px-4 sm:px-6 py-4 shrink-0">
        <div>
          <h1 className="text-xl font-semibold text-zinc-900 dark:text-zinc-100">Insights</h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {summary && summary.total_sessions > 0
              ? `${summary.total_sessions.toLocaleString()} session${summary.total_sessions === 1 ? '' : 's'} analysed`
              : 'Productivity & efficiency metrics for your Claude Code sessions'}
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
          <>
            {/* Productivity Score */}
            <ProductivityCard summary={summary} />

            {/* KPI Cards */}
            <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-5 gap-3">
              <KPICard
                icon={MessageSquare}
                label="Total Sessions"
                value={summary.total_sessions.toLocaleString()}
              />
              <KPICard
                icon={Brain}
                label="Avg Autonomy"
                value={`${Math.round(summary.avg_autonomy_score)} / 100`}
              />
              <KPICard
                icon={TrendingUp}
                label="Avg Turns"
                value={summary.avg_turn_count.toFixed(1)}
              />
              <KPICard
                icon={Wrench}
                label="Avg Tool Calls"
                value={Math.round(summary.avg_tool_calls_total).toLocaleString()}
              />
              <KPICard
                icon={Zap}
                label="Avg Cache Hit"
                value={fmtPct(summary.avg_cache_hit_rate)}
                color="text-amber-600 dark:text-amber-400"
              />
            </div>

            <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-5 gap-3">
              <KPICard
                icon={Clock}
                label="Avg Duration"
                value={fmtMs(summary.avg_total_duration_ms)}
              />
              <KPICard
                icon={DollarSign}
                label="Avg Cost"
                value={fmtUsd(summary.avg_cost_estimate_usd)}
                color="text-emerald-600 dark:text-emerald-400"
              />
              <KPICard
                icon={DollarSign}
                label="Total Cost"
                value={fmtUsd(summary.total_cost_estimate_usd)}
                color="text-emerald-600 dark:text-emerald-400"
              />
              <KPICard
                icon={AlertTriangle}
                label="Sessions w/ Errors"
                value={summary.sessions_with_errors.toLocaleString()}
                color={
                  summary.sessions_with_errors > 0
                    ? 'text-red-600 dark:text-red-400'
                    : 'text-emerald-600 dark:text-emerald-400'
                }
              />
              <KPICard
                icon={Layers}
                label="Error-Free Rate"
                value={fmtPct(
                  (summary.total_sessions - summary.sessions_with_errors) / summary.total_sessions,
                )}
                color="text-emerald-600 dark:text-emerald-400"
              />
            </div>

            {/* Autonomy Gauge + Cache Pie + Error Pie */}
            <div className="grid grid-cols-1 lg:grid-cols-3 gap-5">
              <AutonomyGauge score={summary.avg_autonomy_score} />
              <CacheEfficiencyPie hitRate={summary.avg_cache_hit_rate} />
              <ErrorSessionsPie
                withErrors={summary.sessions_with_errors}
                total={summary.total_sessions}
              />
            </div>

            {/* Top Tools */}
            {summary.top_tools && summary.top_tools.length > 0 && (
              <TopToolsChart tools={summary.top_tools} />
            )}

            {/* Footer note */}
            <p className="text-xs text-zinc-400 dark:text-zinc-500 text-center pb-2">
              Insights are computed from Claude Code session JSONL files and updated incrementally
              in the background. Cost estimates use approximate pricing and may not reflect current
              Anthropic rates.
            </p>
          </>
        ) : (
          <div className="flex flex-col items-center justify-center py-20 gap-2">
            <p className="text-sm text-zinc-500 dark:text-zinc-400">No sessions processed yet.</p>
            <p className="text-xs text-zinc-400 dark:text-zinc-500 text-center max-w-sm">
              Session insights will appear here once Claude Code sessions are scanned and processed
              in the background.
            </p>
          </div>
        )}
      </div>
    </div>
  )
}
