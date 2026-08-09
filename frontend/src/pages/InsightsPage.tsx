import { useState, useEffect, useCallback } from 'react'
import {
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
import { analyticsApi, insightsApi } from '@/lib/api'
import type { AnalyticsReport, InsightCard, InsightSummary, ToolUsageStat } from '@/types'
import {
  RefreshCw,
  Wrench,
  DollarSign,
  Clock,
  AlertTriangle,
  TrendingUp,
  MessageSquare,
  Zap,
} from 'lucide-react'
import { KPICard, ChartCard, DateRangePicker, DatePreset, presetToRange } from './analyticsShared'
import { previousRange } from '@/lib/analyticsMetrics'
import { InsightCardGrid } from './InsightCards'
import { ScanStatusNotice } from './ScanStatusNotice'

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
 * Which way is better for a metric — or that there is no such direction.
 *
 * "neutral" is the important one. Fewer turns can mean a more autonomous run or
 * an abandoned one; a shorter session can mean efficiency or a dead end.
 * Painting those green or red asserts a judgement the number does not support,
 * which is how a dashboard ends up showing "duration −38.6%" in green next to
 * "turns −24%" in red and meaning nothing by either.
 */
type DeltaDirection = 'higher-better' | 'lower-better' | 'neutral'

/**
 * Renders a ±% change badge comparing current against the previous period.
 * Colour is applied only where the direction is defensible.
 */
function DeltaBadge({
  current,
  previous,
  direction = 'neutral',
}: Readonly<{ current: number; previous: number; direction?: DeltaDirection }>) {
  if (previous === 0) return null
  const delta = ((current - previous) / Math.abs(previous)) * 100
  if (Math.abs(delta) < 0.05) return null // ignore sub-0.05% noise

  const positive = delta > 0
  const sign = positive ? '+' : ''
  const good = direction === 'higher-better' ? positive : !positive
  const colorClass =
    direction === 'neutral'
      ? 'text-zinc-500 dark:text-zinc-400 bg-zinc-100 dark:bg-zinc-800'
      : good
        ? 'text-emerald-600 dark:text-emerald-400 bg-emerald-50 dark:bg-emerald-900/30'
        : 'text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/30'

  return (
    <span className={`text-[10px] font-medium px-1 py-0.5 rounded ${colorClass}`}>
      {sign}
      {delta.toFixed(1)}%
    </span>
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

// TopToolsChart renders any tool-call breakdown — by tool, skill, plugin or MCP
// server. All four count the same unit (tool calls), so one chart serves them
// all and the numbers stay comparable across panels.
function TopToolsChart({
  tools,
  prevTools,
  hasComparison,
  title = 'Top 10 Tools Used',
}: Readonly<{
  tools: ToolUsageStat[]
  prevTools?: ToolUsageStat[]
  hasComparison: boolean
  title?: string
}>) {
  // Merge current + previous into a single dataset, top 10 by current count
  const top = tools.slice(0, 10)
  const prevMap = new Map((prevTools ?? []).map(t => [t.tool, t.count]))

  const data: ToolCompareRow[] = top.map(t => ({
    tool: t.tool,
    current: t.count,
    previous: prevMap.get(t.tool) ?? 0,
  }))

  return (
    <ChartCard title={title}>
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
            formatter={(v, name) => [
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
          <Tooltip formatter={v => [`${v ?? 0}%`]} contentStyle={TOOLTIP_STYLE} />
        </PieChart>
      </ResponsiveContainer>
      {prevHitRate !== undefined && (
        <div className="flex justify-center mt-1">
          <DeltaBadge current={hitRate} previous={prevHitRate} direction="higher-better" />
        </div>
      )}
    </ChartCard>
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
  direction?: DeltaDirection
}

function KPIWithDelta({
  icon,
  label,
  value,
  color,
  current,
  previous,
  direction = 'neutral',
}: Readonly<KPIWithDeltaProps>) {
  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700/50 bg-white dark:bg-zinc-900 px-3 py-2.5 flex flex-col gap-1">
      <div className="flex items-center justify-between gap-1">
        <KPICard icon={icon} label={label} value={value} color={color} />
      </div>
      {previous !== undefined && previous !== 0 && (
        <div className="flex justify-start pl-1">
          <DeltaBadge current={current} previous={previous} direction={direction} />
        </div>
      )}
    </div>
  )
}

// ─── Populated content (extracted to keep InsightsPage complexity low) ────────

interface InsightsContentProps {
  summary: InsightSummary
  prevSummary: InsightSummary | null
  hasComparison: boolean
  from: string
  to: string
  cards: InsightCard[]
}

function InsightsContent({
  summary,
  prevSummary,
  hasComparison,
  from,
  to,
  cards,
}: Readonly<InsightsContentProps>) {
  // Use optional chaining via a nullable reference — avoids repeated ternaries.
  const prev = hasComparison ? prevSummary : null

  // Errors per 100 tool calls: a rate with a denominator that grows with the
  // work, unlike "sessions with errors", which counts a session with one
  // failing grep the same as one with fifty broken commands.
  const errorsPerHundred =
    summary.total_tool_calls > 0
      ? (summary.total_tool_errors / summary.total_tool_calls) * 100
      : null

  const prevErrorsPerHundred =
    prev && prev.total_tool_calls > 0
      ? (prev.total_tool_errors / prev.total_tool_calls) * 100
      : null

  const periodDays =
    Math.round((new Date(to).getTime() - new Date(from).getTime()) / 86_400_000) + 1

  return (
    <>
      {/* Specific facts with numbers, in place of a composite 0-100 grade
          built from three unweighted averages and one broken component. */}
      <InsightCardGrid cards={cards} />

      {/* KPI Cards row 1 */}
      <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-5 gap-3">
        <KPICard
          icon={MessageSquare}
          label="Total Sessions"
          value={summary.total_sessions.toLocaleString()}
        />
        <KPIWithDelta
          icon={TrendingUp}
          label="Avg Turns"
          value={summary.avg_turn_count.toFixed(1)}
          current={summary.avg_turn_count}
          previous={prev?.avg_turn_count}
        />
        <KPIWithDelta
          icon={Wrench}
          label="Avg Tool Calls"
          value={Math.round(summary.avg_tool_calls_total).toLocaleString()}
          current={summary.avg_tool_calls_total}
          previous={prev?.avg_tool_calls_total}
        />
        <KPIWithDelta
          icon={Zap}
          label="Avg Cache Hit"
          value={fmtPct(summary.avg_cache_hit_rate)}
          color="text-amber-600 dark:text-amber-400"
          current={summary.avg_cache_hit_rate}
          previous={prev?.avg_cache_hit_rate}
          direction="higher-better"
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
        />
        <KPIWithDelta
          icon={DollarSign}
          label="Avg Cost"
          value={fmtUsd(summary.avg_cost_estimate_usd)}
          color="text-emerald-600 dark:text-emerald-400"
          current={summary.avg_cost_estimate_usd}
          previous={prev?.avg_cost_estimate_usd}
          direction="lower-better"
        />
        <KPICard
          icon={DollarSign}
          label="Total Cost"
          value={fmtUsd(summary.total_cost_estimate_usd)}
          color="text-emerald-600 dark:text-emerald-400"
        />
        {/* One neutral rate instead of "Sessions w/ Errors: 350 (63%!)" in red
            beside "Error-Free Rate 37.2%". A tool_result carrying is_error is
            ordinary agentic behaviour — a grep that matched nothing, a test that
            failed and was then fixed — so flagging whole sessions painted normal
            work as failure. Errors per 100 tool calls scales with how much work
            was done and is comparable between periods. */}
        <KPIWithDelta
          icon={AlertTriangle}
          label="Tool errors / 100 calls"
          value={errorsPerHundred !== null ? errorsPerHundred.toFixed(1) : '—'}
          current={errorsPerHundred ?? 0}
          previous={prevErrorsPerHundred ?? undefined}
          direction="lower-better"
        />
      </div>

      {/* The autonomy gauge and the session-error pie are gone: the first
          presented an uninterpretable formula as a verdict, the second painted
          ordinary tool failures as broken sessions. What remains is a rate a
          reader can act on. */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-5">
        <CacheEfficiencyPie
          hitRate={summary.avg_cache_hit_rate}
          prevHitRate={prev?.avg_cache_hit_rate}
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

      {/* Attribution: which skill, plugin and MCP server drove those tool calls */}
      {summary.top_skills.length > 0 && (
        <>
          <TopToolsChart
            title="Top 10 Skills by Tool Calls"
            tools={summary.top_skills}
            prevTools={prev?.top_skills ?? []}
            hasComparison={hasComparison}
          />
          {/* Without this the skills chart reads as the whole picture, when
              roughly half of all tool calls are made with no skill in context. */}
          {summary.total_tool_calls > 0 && (
            <p className="text-xs text-zinc-400 dark:text-zinc-500 -mt-2 px-1">
              {summary.unattributed_calls.toLocaleString()} of{' '}
              {summary.total_tool_calls.toLocaleString()} tool calls (
              {Math.round((summary.unattributed_calls / summary.total_tool_calls) * 100)}%) were
              made with no skill in context — built-in tool use, not counted above.
            </p>
          )}
        </>
      )}
      {summary.top_plugins.length > 0 && (
        <TopToolsChart
          title="Top 10 Plugins by Tool Calls"
          tools={summary.top_plugins}
          prevTools={prev?.top_plugins ?? []}
          hasComparison={hasComparison}
        />
      )}
      {summary.top_mcp_servers.length > 0 && (
        <TopToolsChart
          title="Top 10 MCP Servers by Tool Calls"
          tools={summary.top_mcp_servers}
          prevTools={prev?.top_mcp_servers ?? []}
          hasComparison={hasComparison}
        />
      )}
      {/* Immediately after the servers chart: this is that chart's drill-down,
          the same calls counted by tool rather than by server. */}
      {summary.top_mcp_tools.length > 0 && (
        <TopToolsChart
          title="Top 10 MCP Tools by Tool Calls"
          tools={summary.top_mcp_tools}
          prevTools={prev?.top_mcp_tools ?? []}
          hasComparison={hasComparison}
        />
      )}
      {/* Delegation mix. Empty until a session delegates, since the agent is
          stamped on sub-agent transcripts only. */}
      {summary.top_agents.length > 0 && (
        <TopToolsChart
          title="Top 10 Sub-agents by Tool Calls"
          tools={summary.top_agents}
          prevTools={prev?.top_agents ?? []}
          hasComparison={hasComparison}
        />
      )}
      {/* A single-category bar chart is a full-height panel carrying no
          information — on most corpora every call runs at one effort tier. */}
      {summary.top_efforts.length > 1 && (
        <TopToolsChart
          title="Tool Calls by Reasoning Effort"
          tools={summary.top_efforts}
          prevTools={prev?.top_efforts ?? []}
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
  const [report, setReport] = useState<AnalyticsReport | null>(null)
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [preset, setPreset] = useState<DatePreset>('30d')
  const [from, setFrom] = useState(() => presetToRange('30d').from)
  const [to, setTo] = useState(() => presetToRange('30d').to)
  const [project, setProject] = useState('all')

  const isAllTime = preset === 'all-time'

  // The analytics report is fetched alongside the insight summary for two
  // things this page cannot get from the summary: the actionable cards (which
  // need per-model cost and the pricing catalog) and the project list for the
  // picker. Both endpoints now take the same window and project parameters and
  // filter through the same code, so the two halves of the page describe one
  // set of sessions.
  // The window is always sent, including for "All time" — which presetToRange
  // expresses as 2020-01-01 onward, the same convention the other two
  // dashboards use. Omitting the dates used to mean "unbounded" but now means
  // "the last 30 days", the endpoint's default, which would have silently
  // narrowed this page's KPIs while its cards still covered everything.
  const load = useCallback(async (f: string, t: string, allTime: boolean, proj: string) => {
    const scope = proj === 'all' ? undefined : proj
    try {
      const [curr, prev, analytics] = await Promise.all([
        insightsApi.getSummary({ from: f, to: t, project: scope }),
        allTime
          ? Promise.resolve(null)
          : insightsApi.getSummary({ ...previousRange(f, t), project: scope }),
        analyticsApi.get({ from: f, to: t, project: scope }),
      ])
      setSummary(curr)
      setPrevSummary(prev)
      setReport(analytics)
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load insights')
    } finally {
      setLoading(false)
      setRefreshing(false)
    }
  }, [])

  useEffect(() => {
    void load(from, to, isAllTime, project)
  }, [load, from, to, isAllTime, project])

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
    void load(from, to, isAllTime, project)
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
          projects={report?.projects}
          project={project}
          onProject={setProject}
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
        <ScanStatusNotice onSettled={handleRefresh} />

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
            cards={report?.insight_cards ?? []}
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
