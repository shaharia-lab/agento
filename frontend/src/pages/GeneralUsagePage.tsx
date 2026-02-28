import { useState, useEffect, useCallback } from 'react'
import {
  AreaChart,
  Area,
  BarChart,
  Bar,
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
  ModelSessionStat,
  HeatmapCell,
  HourlyActivity,
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
} from './analyticsShared'

// ─── Charts ───────────────────────────────────────────────────────────────────

function SessionsTimeSeriesChart({ data }: Readonly<{ data: TimeSeriesPoint[] }>) {
  const formatted = data.map(d => ({ ...d, date: formatDateLabel(d.date) }))
  return (
    <ChartCard title="Sessions Over Time">
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
            allowDecimals={false}
            tick={{ fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            width={36}
          />
          <Tooltip
            formatter={(v: number | undefined) => [v ?? 0, 'Sessions']}
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
        </AreaChart>
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
            {formatted.map((_, i) => (
              <Cell key={i} fill={MODEL_COLORS[i % MODEL_COLORS.length]} />
            ))}
          </Bar>
        </BarChart>
      </ResponsiveContainer>
    </ChartCard>
  )
}

function ActivityHeatmap({ data }: Readonly<{ data: HeatmapCell[] }>) {
  const grid: number[][] = Array.from({ length: 7 }, () => new Array(24).fill(0))
  let maxSessions = 0
  for (const cell of data) {
    grid[cell.day_of_week][cell.hour] = cell.sessions
    if (cell.sessions > maxSessions) maxSessions = cell.sessions
  }

  const cellMap = new Map(data.map(c => [`${c.day_of_week}-${c.hour}`, c]))

  return (
    <ChartCard title="Activity Heatmap (Day × Hour)">
      <div className="overflow-x-auto">
        <div className="min-w-[560px]">
          {/* Hour labels */}
          <div className="flex ml-8 mb-1">
            {Array.from({ length: 24 }, (_, h) => (
              <div
                key={h}
                className="flex-1 text-center text-[9px] text-zinc-400 dark:text-zinc-500"
              >
                {h % 3 === 0 ? h : ''}
              </div>
            ))}
          </div>
          {/* Rows */}
          {DAY_NAMES.map((day, dow) => (
            <div key={dow} className="flex items-center mb-0.5">
              <span className="w-8 text-[10px] text-zinc-400 dark:text-zinc-500 shrink-0">
                {day}
              </span>
              {Array.from({ length: 24 }, (_, h) => {
                const cell = cellMap.get(`${dow}-${h}`)
                const intensity = maxSessions > 0 ? (cell?.sessions ?? 0) / maxSessions : 0
                const bg =
                  intensity === 0
                    ? 'bg-zinc-100 dark:bg-zinc-800'
                    : intensity < 0.25
                      ? 'bg-indigo-200 dark:bg-indigo-900/60'
                      : intensity < 0.5
                        ? 'bg-indigo-400 dark:bg-indigo-700'
                        : intensity < 0.75
                          ? 'bg-indigo-600 dark:bg-indigo-500'
                          : 'bg-indigo-800 dark:bg-indigo-400'
                return (
                  <div
                    key={h}
                    className={`flex-1 aspect-square rounded-[2px] mx-px ${bg} cursor-default`}
                    title={
                      cell
                        ? `${day} ${h}:00 — ${cell.sessions} sessions, ${formatTokens(cell.tokens)} tokens`
                        : `${day} ${h}:00 — no activity`
                    }
                  />
                )
              })}
            </div>
          ))}
          {/* Legend */}
          <div className="flex items-center gap-1 mt-2 ml-8">
            <span className="text-[10px] text-zinc-400 dark:text-zinc-500 mr-1">Less</span>
            {[
              'bg-zinc-100 dark:bg-zinc-800',
              'bg-indigo-200 dark:bg-indigo-900/60',
              'bg-indigo-400 dark:bg-indigo-700',
              'bg-indigo-600 dark:bg-indigo-500',
              'bg-indigo-800 dark:bg-indigo-400',
            ].map((cls, i) => (
              <div key={i} className={`w-3 h-3 rounded-[2px] ${cls}`} />
            ))}
            <span className="text-[10px] text-zinc-400 dark:text-zinc-500 ml-1">More</span>
          </div>
        </div>
      </div>
    </ChartCard>
  )
}

function HourlyActivityChart({ data }: Readonly<{ data: HourlyActivity[] }>) {
  return (
    <ChartCard title="Activity by Hour of Day">
      <ResponsiveContainer width="100%" height={240}>
        <BarChart data={data} margin={{ top: 4, right: 8, left: 0, bottom: 0 }}>
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
            formatter={(v: number | undefined) => [v ?? 0, 'Sessions']}
            labelFormatter={h => `Hour ${h}:00`}
            contentStyle={{ fontSize: 12, borderRadius: 6 }}
          />
          <Bar dataKey="sessions" name="Sessions" fill="#22c55e" radius={[2, 2, 0, 0]} />
        </BarChart>
      </ResponsiveContainer>
    </ChartCard>
  )
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

function avgSessionsPerDay(totalSessions: number, fromDate: string, toDate: string): string {
  const days = Math.max(
    1,
    Math.round((new Date(toDate).getTime() - new Date(fromDate).getTime()) / 86_400_000) + 1,
  )
  const avg = totalSessions / days
  return avg < 1 ? avg.toFixed(2) : avg.toFixed(1)
}

// ─── Main Page ────────────────────────────────────────────────────────────────

export default function GeneralUsagePage() {
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
          <h1 className="text-base font-semibold text-zinc-900 dark:text-zinc-100">
            General Usage
          </h1>
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
            <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
              <KPICard
                icon={Hash}
                label="Total Sessions"
                value={summary.total_sessions.toLocaleString()}
              />
              <KPICard
                icon={CalendarDays}
                label="Avg Sessions / Day"
                value={avgSessionsPerDay(summary.total_sessions, from, to)}
              />
              <KPICard
                icon={Clock}
                label="Top Model"
                value={formatModelName(summary.most_used_model)}
              />
              <KPICard
                icon={Activity}
                label="Unique Projects"
                value={String((report?.projects?.length ?? 0) || '—')}
              />
            </div>

            {/* Sessions over time */}
            <SessionsTimeSeriesChart data={report?.time_series ?? []} />

            {/* Sessions per model */}
            {(report?.sessions_per_model?.length ?? 0) > 0 && (
              <SessionsPerModelChart data={report!.sessions_per_model} />
            )}

            {/* Heatmap + Hourly */}
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-5">
              <ActivityHeatmap data={report?.heatmap ?? []} />
              <HourlyActivityChart data={report?.hourly_activity ?? []} />
            </div>
          </>
        )}
      </div>
    </div>
  )
}
