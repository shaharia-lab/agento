import { useState, useEffect, useCallback, useRef } from 'react'
import { useNavigate } from 'react-router-dom'
import {
  Area,
  BarChart,
  Bar,
  ComposedChart,
  Line,
  Cell,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
  ResponsiveContainer,
} from 'recharts'
import { analyticsApi } from '@/lib/api'
import {
  avgSessionsPerDay,
  observedDaySpan,
  previousRange,
  withPreviousSeries,
} from '@/lib/analyticsMetrics'
import { ProjectAnalytics } from './ProjectAnalytics'
import { TopSessionsCard } from './TopSessionsCard'
import { ScanStatusNotice } from './ScanStatusNotice'
import {
  drilldownUrl,
  heatmapCellTarget,
  hourlyBarTarget,
  parseRangeBounds,
  MAX_DRILLDOWN_DAYS,
} from '@/lib/drilldown'
import type {
  AnalyticsReport,
  AnalyticsSummary,
  TimeSeriesPoint,
  ModelSessionStat,
  HeatmapCell,
  HourlyActivity,
  DayActivity,
} from '@/types'
import { RefreshCw, Hash, Clock, Activity, CalendarDays } from 'lucide-react'
import {
  MODEL_COLORS,
  DAY_NAMES,
  DatePreset,
  presetToRange,
  formatTokens,
  formatModelName,
  formatDateLabel,
  KPICard,
  ChartCard,
  DateRangePicker,
  CompareToggle,
} from './analyticsShared'

// ─── Charts ───────────────────────────────────────────────────────────────────

function SessionsTimeSeriesChart({
  data,
  previous,
}: Readonly<{ data: TimeSeriesPoint[]; previous?: TimeSeriesPoint[] }>) {
  // Aligned by bucket position: the first day of this window against the first
  // day of the previous one, which is the comparison being asked for.
  const hasGhost = (previous?.length ?? 0) > 0
  const formatted = withPreviousSeries(
    data,
    previous,
    d => ({ ...d, date: formatDateLabel(d.date) }),
    d => d.sessions,
  )
  return (
    <ChartCard
      title="Sessions Over Time"
      subtitle={
        hasGhost
          ? 'Dashed line is the equally-sized window immediately before this one.'
          : undefined
      }
    >
      {/* ComposedChart rather than AreaChart: the ghost is a line over an area,
          and an <Area> with no fill does not reliably render as one. */}
      <ResponsiveContainer width="100%" height={280}>
        <ComposedChart data={formatted} margin={{ top: 4, right: 8, left: 0, bottom: 0 }}>
          <CartesianGrid strokeDasharray="3 3" stroke="#27272a" strokeOpacity={0.5} />
          <XAxis
            dataKey="date"
            tick={{ fontSize: 11 }}
            tickLine={false}
            interval="preserveStartEnd"
          />
          <YAxis
            allowDecimals={false}
            tick={{ fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            width={36}
          />
          <Tooltip
            formatter={v => [v ?? 0, 'Sessions']}
            contentStyle={{ fontSize: 12, borderRadius: 6 }}
          />
          <Legend wrapperStyle={{ fontSize: 12 }} />
          <Area
            type="monotone"
            dataKey="sessions"
            name="Sessions"
            stroke="#6366f1"
            fill="#6366f1"
            fillOpacity={0.15}
            strokeWidth={1.5}
          />
          {hasGhost && (
            <Line
              type="monotone"
              dataKey="previous"
              name="Previous period"
              stroke="#a1a1aa"
              strokeDasharray="4 3"
              strokeWidth={1.5}
              dot={false}
            />
          )}
        </ComposedChart>
      </ResponsiveContainer>
    </ChartCard>
  )
}

function SessionsPerModelChart({ data }: Readonly<{ data: ModelSessionStat[] }>) {
  const formatted = data.map(d => ({ ...d, model: formatModelName(d.model) }))
  return (
    <ChartCard title="Sessions per Model">
      <ResponsiveContainer width="100%" height={280}>
        <BarChart
          data={formatted}
          layout="vertical"
          margin={{ top: 4, right: 16, left: 8, bottom: 0 }}
        >
          <CartesianGrid
            strokeDasharray="3 3"
            stroke="#27272a"
            strokeOpacity={0.5}
            horizontal={false}
          />
          <XAxis type="number" tick={{ fontSize: 11 }} tickLine={false} allowDecimals={false} />
          <YAxis
            type="category"
            dataKey="model"
            tick={{ fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            width={90}
          />
          <Tooltip contentStyle={{ fontSize: 12, borderRadius: 6 }} />
          <Bar dataKey="sessions" name="Sessions" radius={[0, 2, 2, 0]}>
            {formatted.map((entry, i) => (
              <Cell key={`model-${entry.model}`} fill={MODEL_COLORS[i % MODEL_COLORS.length]} />
            ))}
          </Bar>
        </BarChart>
      </ResponsiveContainer>
    </ChartCard>
  )
}

function heatmapCellBg(intensity: number): string {
  if (intensity === 0) return 'bg-zinc-100 dark:bg-zinc-800'
  if (intensity < 0.25) return 'bg-indigo-200 dark:bg-indigo-900/60'
  if (intensity < 0.5) return 'bg-indigo-400 dark:bg-indigo-700'
  if (intensity < 0.75) return 'bg-indigo-600 dark:bg-indigo-500'
  return 'bg-indigo-800 dark:bg-indigo-400'
}

const ARROW_MOVES: Record<string, [number, number]> = {
  ArrowRight: [0, 1],
  ArrowLeft: [0, -1],
  ArrowDown: [1, 0],
  ArrowUp: [-1, 0],
}

function cellTitle(day: string, hour: number, cell: HeatmapCell | undefined): string {
  if (!cell) return `${day} ${hour}:00, no activity`
  return `${day} ${hour}:00, ${cell.sessions} sessions, ${formatTokens(cell.tokens)} tokens. Click to view sessions`
}

interface HeatmapCellCommonProps {
  day: string
  dow: number
  hour: number
  cell: HeatmapCell | undefined
  maxSessions: number
}

function StaticHeatmapCell({ day, hour, cell, maxSessions }: Readonly<HeatmapCellCommonProps>) {
  const intensity = maxSessions > 0 ? (cell?.sessions ?? 0) / maxSessions : 0
  return (
    <div
      className={`flex-1 aspect-square rounded-[2px] mx-px ${heatmapCellBg(intensity)} cursor-default`}
      title={
        cell
          ? cellTitle(day, hour, cell).replace('. Click to view sessions', '')
          : `${day} ${hour}:00, no activity`
      }
    />
  )
}

function InteractiveHeatmapCell({
  day,
  dow,
  hour,
  cell,
  maxSessions,
  focused,
  registerRef,
  onCellClick,
  onMoveFocus,
}: Readonly<
  HeatmapCellCommonProps & {
    focused: boolean
    registerRef: (key: string, el: HTMLDivElement | null) => void
    onCellClick: (dayOfWeek: number, hour: number) => void
    onMoveFocus: (dayOfWeek: number, hour: number) => void
  }
>) {
  const intensity = maxSessions > 0 ? (cell?.sessions ?? 0) / maxSessions : 0
  const clickable = !!cell

  const handleKeyDown = (e: React.KeyboardEvent) => {
    const move = ARROW_MOVES[e.key]
    if (move) {
      e.preventDefault()
      onMoveFocus(dow + move[0], hour + move[1])
      return
    }
    if ((e.key === 'Enter' || e.key === ' ') && clickable) {
      e.preventDefault()
      onCellClick(dow, hour)
    }
  }

  return (
    <div
      ref={el => registerRef(`${dow}-${hour}`, el)}
      role="gridcell"
      aria-label={`${day} ${hour}:00, ${cell ? `${cell.sessions} sessions` : 'no activity'}`}
      tabIndex={focused ? 0 : -1}
      onClick={clickable ? () => onCellClick(dow, hour) : undefined}
      onKeyDown={handleKeyDown}
      className={`flex-1 aspect-square rounded-[2px] mx-px ${heatmapCellBg(intensity)} focus:outline-none focus:ring-2 focus:ring-indigo-500 ${
        clickable ? 'cursor-pointer hover:ring-1 hover:ring-indigo-500' : 'cursor-default'
      }`}
      title={cellTitle(day, hour, cell)}
    />
  )
}

function HeatmapGridCell({
  interactive,
  ...props
}: Readonly<
  HeatmapCellCommonProps & {
    interactive: boolean
    focused: boolean
    registerRef: (key: string, el: HTMLDivElement | null) => void
    onCellClick?: (dayOfWeek: number, hour: number) => void
    onMoveFocus: (dayOfWeek: number, hour: number) => void
  }
>) {
  if (!interactive) return <StaticHeatmapCell {...props} />
  return <InteractiveHeatmapCell {...props} onCellClick={props.onCellClick ?? (() => {})} />
}

function ActivityHeatmap({
  data,
  onCellClick,
}: Readonly<{ data: HeatmapCell[]; onCellClick?: (dayOfWeek: number, hour: number) => void }>) {
  let maxSessions = 0
  for (const cell of data) {
    if (cell.sessions > maxSessions) maxSessions = cell.sessions
  }

  const cellMap = new Map(data.map(c => [`${c.day_of_week}-${c.hour}`, c]))

  // Roving tabindex: only the focused cell is in the tab order; arrows move
  // focus across the grid (WAI-ARIA grid pattern).
  const interactive = !!onCellClick
  const [focusPos, setFocusPos] = useState({ dow: 0, hour: 0 })
  const cellRefs = useRef(new Map<string, HTMLDivElement | null>())
  const registerRef = useCallback((key: string, el: HTMLDivElement | null) => {
    cellRefs.current.set(key, el)
  }, [])
  const moveFocus = useCallback((dow: number, hour: number) => {
    const next = {
      dow: Math.min(6, Math.max(0, dow)),
      hour: Math.min(23, Math.max(0, hour)),
    }
    setFocusPos(next)
    cellRefs.current.get(`${next.dow}-${next.hour}`)?.focus()
  }, [])

  return (
    <ChartCard
      title="Activity Heatmap (Day × Hour)"
      subtitle="A session counts in every hour between its start and last activity, so an eight-hour session shades eight cells. It used to shade only the hour it ended, making this a map of when work stopped. A session resumed after a break counts the gap too."
    >
      <div className="overflow-x-auto">
        <div
          className="min-w-[560px]"
          role={interactive ? 'grid' : undefined}
          aria-label={interactive ? 'Activity by day and hour' : undefined}
        >
          {/* Hour labels */}
          <div className="flex ml-8 mb-1" role={interactive ? 'presentation' : undefined}>
            {Array.from({ length: 24 }, (_, h) => (
              <div
                key={`hour-${h}`}
                className="flex-1 text-center text-[11px] text-zinc-400 dark:text-zinc-500"
              >
                {h % 3 === 0 ? h : ''}
              </div>
            ))}
          </div>
          {/* Rows */}
          {DAY_NAMES.map((day, dow) => (
            <div
              key={`day-${day}`}
              className="flex items-center mb-0.5"
              role={interactive ? 'row' : undefined}
            >
              <span
                className="w-8 text-[12px] text-zinc-400 dark:text-zinc-500 shrink-0"
                role={interactive ? 'rowheader' : undefined}
              >
                {day}
              </span>
              {Array.from({ length: 24 }, (_, h) => (
                <HeatmapGridCell
                  key={`cell-${dow}-${h}`}
                  day={day}
                  dow={dow}
                  hour={h}
                  cell={cellMap.get(`${dow}-${h}`)}
                  maxSessions={maxSessions}
                  interactive={interactive}
                  focused={focusPos.dow === dow && focusPos.hour === h}
                  registerRef={registerRef}
                  onCellClick={onCellClick}
                  onMoveFocus={moveFocus}
                />
              ))}
            </div>
          ))}
          {/* Legend */}
          <div className="flex items-center gap-1 mt-2 ml-8">
            <span className="text-[12px] text-zinc-400 dark:text-zinc-500 mr-1">Less</span>
            {[
              'bg-zinc-100 dark:bg-zinc-800',
              'bg-indigo-200 dark:bg-indigo-900/60',
              'bg-indigo-400 dark:bg-indigo-700',
              'bg-indigo-600 dark:bg-indigo-500',
              'bg-indigo-800 dark:bg-indigo-400',
            ].map(cls => (
              <div key={cls} className={`w-3 h-3 rounded-[2px] ${cls}`} />
            ))}
            <span className="text-[12px] text-zinc-400 dark:text-zinc-500 ml-1">More</span>
          </div>
          {onCellClick && (
            <p className="text-[11px] text-zinc-400 dark:text-zinc-500 mt-1.5 ml-8">
              Click a cell to view the sessions from that day and hour. Keyboard: tab to the grid,
              arrow keys to move, Enter to open.
            </p>
          )}
        </div>
      </div>
    </ChartCard>
  )
}

function HourlyActivityChart({
  data,
  onBarClick,
}: Readonly<{ data: HourlyActivity[]; onBarClick?: (hour: number) => void }>) {
  // Per-bar click handler: recharts 3 does not reliably populate activePayload
  // in chart-level onClick state, but Bar's own onClick receives the bar's
  // data entry directly.
  const handleBarClick = onBarClick
    ? (entry: unknown) => {
        const hour = (entry as { hour?: unknown })?.hour
        if (typeof hour === 'number' && data.find(d => d.hour === hour)?.sessions) {
          onBarClick(hour)
        }
      }
    : undefined

  return (
    <ChartCard
      title="Activity by Hour of Day"
      subtitle="Sessions counted in every hour between their start and last activity. Totals exceed the session count because one session spans several hours, and a session resumed after a break counts the gap."
    >
      <ResponsiveContainer width="100%" height={240}>
        <BarChart
          data={data}
          margin={{ top: 4, right: 8, left: 0, bottom: 0 }}
          style={onBarClick ? { userSelect: 'none' } : undefined}
        >
          <CartesianGrid strokeDasharray="3 3" stroke="#27272a" strokeOpacity={0.5} />
          <XAxis
            dataKey="hour"
            tickFormatter={h => `${h}:00`}
            tick={{ fontSize: 10 }}
            tickLine={false}
            interval={2}
          />
          <YAxis
            tick={{ fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            width={32}
            allowDecimals={false}
          />
          <Tooltip
            formatter={v => [v ?? 0, 'Sessions']}
            labelFormatter={h => `Hour ${h}:00`}
            contentStyle={{ fontSize: 12, borderRadius: 6 }}
          />
          <Bar
            dataKey="sessions"
            name="Sessions"
            fill="#22c55e"
            radius={[2, 2, 0, 0]}
            cursor={onBarClick ? 'pointer' : undefined}
            onClick={handleBarClick}
          />
        </BarChart>
      </ResponsiveContainer>
      {onBarClick && (
        <p className="text-[11px] text-zinc-400 dark:text-zinc-500 mt-1.5">
          Click a bar to view the sessions from that hour.
        </p>
      )}
    </ChartCard>
  )
}

/**
 * The busiest days in the window.
 *
 * most_active_days has shipped in every analytics response since the endpoint
 * existed and was never rendered — computed, sorted and thrown away. It answers
 * "when did the work actually happen" at a glance, which the heatmap answers
 * only by shape.
 */
function MostActiveDays({ days }: Readonly<{ days: DayActivity[] }>) {
  if (days.length === 0) return null
  const top = days.slice(0, 10)
  const max = top[0]?.tokens || 1

  return (
    <ChartCard title="Busiest Days" subtitle="Ranked by conversation tokens.">
      <ul className="space-y-2">
        {top.map((day, i) => (
          <li key={day.date} className="text-xs">
            <div className="flex items-baseline justify-between mb-1">
              <span className="text-zinc-700 dark:text-zinc-300">
                {formatDateLabel(day.date)}
                <span className="text-zinc-400 dark:text-zinc-500">
                  {' '}
                  · {day.sessions} session{day.sessions === 1 ? '' : 's'}
                </span>
              </span>
              <span className="tabular-nums text-zinc-500 dark:text-zinc-400">
                {formatTokens(day.tokens)}
              </span>
            </div>
            <div className="h-1.5 rounded-full bg-zinc-100 dark:bg-zinc-800 overflow-hidden">
              <div
                className="h-full rounded-full"
                style={{
                  width: `${(day.tokens / max) * 100}%`,
                  backgroundColor: MODEL_COLORS[i % MODEL_COLORS.length],
                }}
              />
            </div>
          </li>
        ))}
      </ul>
    </ChartCard>
  )
}

/**
 * The populated page body, extracted so the page component stays readable —
 * the same split InsightsPage uses for the same reason.
 */
function GeneralUsageContent({
  report,
  summary,
  observedSpan,
  compare,
  prevReport,
  drilldownEnabled,
  onDrill,
}: Readonly<{
  report: AnalyticsReport | null
  summary: AnalyticsSummary
  observedSpan: number
  compare: boolean
  prevReport: AnalyticsReport | null
  drilldownEnabled: boolean
  onDrill: (dayOfWeek: number | null, hour: number) => void
}>) {
  return (
    <>
      {/* KPI Cards */}
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
        <KPICard
          icon={Hash}
          label="Total Sessions"
          value={summary.total_sessions.toLocaleString()}
        />
        <KPICard
          icon={CalendarDays}
          label="Avg Sessions / Day"
          value={avgSessionsPerDay(
            summary.total_sessions,
            report?.time_series ?? [],
            report?.granularity,
          )}
          sub={
            observedSpan > 0
              ? `over ${observedSpan} day${observedSpan === 1 ? '' : 's'} with activity`
              : undefined
          }
        />
        <KPICard icon={Clock} label="Top Model" value={formatModelName(summary.most_used_model)} />
        {/* summary.unique_projects, not report.projects.length: the
                  latter is the picker's option list and is built before
                  filtering, so it ignored both the window and the project
                  filter. */}
        <KPICard
          icon={Activity}
          label="Unique Projects"
          value={String(summary.unique_projects || '—')}
        />
      </div>

      {/* Sessions over time */}
      <SessionsTimeSeriesChart
        data={report?.time_series ?? []}
        previous={compare && prevReport ? prevReport.time_series : undefined}
      />

      {/* Sessions per model */}
      {(report?.sessions_per_model?.length ?? 0) > 0 && (
        <SessionsPerModelChart data={report!.sessions_per_model} />
      )}

      {/* Heatmap + Hourly */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-5">
        <ActivityHeatmap
          data={report?.heatmap ?? []}
          onCellClick={drilldownEnabled ? (dow, hour) => onDrill(dow, hour) : undefined}
        />
        <HourlyActivityChart
          data={report?.hourly_activity ?? []}
          onBarClick={drilldownEnabled ? hour => onDrill(null, hour) : undefined}
        />
      </div>
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-5">
        <MostActiveDays days={report?.most_active_days ?? []} />
        {report?.top_sessions && <TopSessionsCard top={report.top_sessions} />}
      </div>

      {/* Projects and leaderboards: the two questions the dashboards
                could not answer at all, rather than answered wrongly. */}
      <ProjectAnalytics
        projects={report?.project_breakdown ?? []}
        activity={report?.project_activity ?? []}
      />

      {!drilldownEnabled && (
        <p className="text-[11px] text-zinc-400 dark:text-zinc-500 -mt-3">
          Session drill-down is available for ranges up to {MAX_DRILLDOWN_DAYS} days. Pick a
          narrower date range to click through to sessions.
        </p>
      )}
    </>
  )
}

// ─── Main Page ────────────────────────────────────────────────────────────────

export default function GeneralUsagePage() {
  const navigate = useNavigate()
  const [report, setReport] = useState<AnalyticsReport | null>(null)
  const [prevReport, setPrevReport] = useState<AnalyticsReport | null>(null)
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [preset, setPreset] = useState<DatePreset>('30d')
  const [from, setFrom] = useState(() => presetToRange('30d').from)
  const [to, setTo] = useState(() => presetToRange('30d').to)
  const [project, setProject] = useState('all')
  const [compare, setCompare] = useState(false)

  const load = useCallback(async (f: string, t: string, proj: string, withPrevious: boolean) => {
    try {
      const scope = proj === 'all' ? undefined : proj
      const prevRange = previousRange(f, t)
      const [data, prior] = await Promise.all([
        analyticsApi.get({ from: f, to: t, project: scope }),
        withPrevious
          ? analyticsApi.get({ from: prevRange.from, to: prevRange.to, project: scope })
          : Promise.resolve(null),
      ])
      setReport(data)
      setPrevReport(prior)
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load analytics')
    } finally {
      setLoading(false)
      setRefreshing(false)
    }
  }, [])

  useEffect(() => {
    load(from, to, project, compare)
  }, [load, from, to, project, compare])

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
    load(from, to, project, compare)
  }

  // Beyond MAX_DRILLDOWN_DAYS the serialized window list would exceed URL
  // length limits, so charts render without click-through.
  const drilldownEnabled = parseRangeBounds(from, to) !== null

  const drillIntoSessions = useCallback(
    (dayOfWeek: number | null, hour: number) => {
      const target =
        dayOfWeek === null
          ? hourlyBarTarget(from, to, hour)
          : heatmapCellTarget(from, to, dayOfWeek, hour)
      if (target && target.windows.length > 0)
        navigate(drilldownUrl(target, project === 'all' ? undefined : project))
    },
    [from, to, project, navigate],
  )

  const observedSpan = observedDaySpan(report?.time_series ?? [], report?.granularity)

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
          <h1 className="text-xl font-semibold text-zinc-900 dark:text-zinc-100">General Usage</h1>
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
        <div className="mt-2">
          <CompareToggle
            enabled={compare}
            onChange={setCompare}
            label="Overlay the equally-sized window immediately before this one"
          />
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto px-4 sm:px-6 py-5 space-y-5">
        <ScanStatusNotice onSettled={handleRefresh} />

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
          <GeneralUsageContent
            report={report}
            summary={summary}
            observedSpan={observedSpan}
            compare={compare}
            prevReport={prevReport}
            drilldownEnabled={drilldownEnabled}
            onDrill={drillIntoSessions}
          />
        )}
      </div>
    </div>
  )
}
