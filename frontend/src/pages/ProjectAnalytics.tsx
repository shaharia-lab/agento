/**
 * Project analytics: which projects the money and the time went to, and which
 * days each was worked on.
 *
 * The project was previously a filter and nothing else — no view anywhere
 * aggregated by it, so "which of my projects is expensive?" and "what was I
 * working on that week?" had no answer. Both are computed from project_path,
 * which is already on every cached row.
 */
import { formatCost } from '@/lib/format'
import type { ProjectDayActivity, ProjectStat } from '@/types'

import { ChartCard, formatTokens } from './analyticsShared'

/** Last two path segments — a full path is unreadable in a table cell. */
export function shortProject(path: string): string {
  if (!path) return 'unknown'
  const parts = path.split('/').filter(Boolean)
  return parts.slice(-2).join('/') || path
}

/**
 * Names the folded tail row.
 *
 * Beyond the top 20 the backend sums the remaining projects into one row rather
 * than dropping them, so the table's total stays the window's total. Stating
 * how many it stands for is the point: a table showing 20 of 500 rows without
 * saying so reads as the whole picture.
 */
function projectLabel(p: ProjectStat): string {
  if (!p.folded_projects) return shortProject(p.project)
  return `${p.project} (${p.folded_projects})`
}

function ProjectTable({ projects }: Readonly<{ projects: ProjectStat[] }>) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs">
        <thead>
          <tr className="border-b border-zinc-200 dark:border-zinc-700/50 text-zinc-500 dark:text-zinc-400">
            <th className="text-left font-medium py-1.5 pr-4">Project</th>
            <th className="text-right font-medium py-1.5 pr-4">Sessions</th>
            <th className="text-right font-medium py-1.5 pr-4">Conversation tokens</th>
            <th className="text-right font-medium py-1.5 pr-4">Cost</th>
            <th className="text-right font-medium py-1.5">Share</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-zinc-100 dark:divide-zinc-700/30">
          {projects.map(p => (
            <tr key={p.project}>
              <td
                className={`py-1.5 pr-4 text-zinc-700 dark:text-zinc-300 ${
                  p.folded_projects ? 'italic text-zinc-500 dark:text-zinc-400' : 'font-mono'
                }`}
                title={
                  p.folded_projects ? `${p.folded_projects} further projects, summed` : p.project
                }
              >
                {projectLabel(p)}
              </td>
              <td className="py-1.5 pr-4 text-right tabular-nums">{p.sessions}</td>
              <td className="py-1.5 pr-4 text-right tabular-nums">{formatTokens(p.tokens)}</td>
              <td className="py-1.5 pr-4 text-right tabular-nums font-medium text-zinc-900 dark:text-zinc-100">
                {formatCost(p.cost.total_usd)}
              </td>
              <td className="py-1.5 text-right tabular-nums text-zinc-500 dark:text-zinc-400">
                {p.percentage.toFixed(1)}%
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

/**
 * A project × time strip: one row per project, one cell per bucket, shaded by
 * that bucket's spend.
 *
 * The bucket is whatever granularity the report was built at — a day at the
 * windows a reader normally looks at, a week or a month on a multi-year one,
 * which is what keeps the strip from growing with the calendar. The cells are
 * laid out from the dates actually present, so no width is assumed here.
 *
 * Spend rather than session count, because a bucket with one expensive session
 * is the one a reader is looking for. The scale is per-strip so the busiest cell
 * is always full-strength; comparing absolute shades across two different
 * windows is not something this chart claims to support.
 */
function ProjectDayStrip({
  activity,
  projects,
}: Readonly<{ activity: ProjectDayActivity[]; projects: ProjectStat[] }>) {
  const dates = [...new Set(activity.map(a => a.date))].sort((a, b) => a.localeCompare(b))
  const charted = projects.filter(p => activity.some(a => a.project === p.project))
  const byKey = new Map(activity.map(a => [`${a.project}|${a.date}`, a]))
  const max = Math.max(...activity.map(a => a.cost_usd), 0)

  if (dates.length === 0) return null

  const shade = (cost: number) => {
    if (cost <= 0) return 'bg-zinc-100 dark:bg-zinc-800'
    const ratio = max > 0 ? cost / max : 0
    if (ratio < 0.25) return 'bg-indigo-200 dark:bg-indigo-900/60'
    if (ratio < 0.5) return 'bg-indigo-400 dark:bg-indigo-700'
    if (ratio < 0.75) return 'bg-indigo-600 dark:bg-indigo-500'
    return 'bg-indigo-800 dark:bg-indigo-400'
  }

  return (
    <div className="overflow-x-auto">
      <div className="min-w-[560px] space-y-1">
        {charted.map(p => (
          <div key={p.project} className="flex items-center gap-2">
            <span
              className="w-40 shrink-0 truncate text-[11px] font-mono text-zinc-500 dark:text-zinc-400"
              title={p.project}
            >
              {shortProject(p.project)}
            </span>
            <div className="flex flex-1 gap-px">
              {dates.map(date => {
                const cell = byKey.get(`${p.project}|${date}`)
                const cost = cell?.cost_usd ?? 0
                return (
                  <div
                    key={date}
                    className={`h-4 flex-1 rounded-[2px] ${shade(cost)}`}
                    title={
                      cell
                        ? `${shortProject(p.project)} · ${date} — ${cell.sessions} session${cell.sessions === 1 ? '' : 's'}, ${formatCost(cost)}`
                        : `${shortProject(p.project)} · ${date} — no activity`
                    }
                  />
                )
              })}
            </div>
          </div>
        ))}
        <div className="flex items-center gap-2 pt-1">
          <span className="w-40 shrink-0" />
          <div className="flex flex-1 justify-between text-[10px] text-zinc-400 dark:text-zinc-500">
            <span>{dates[0]}</span>
            <span>{dates[dates.length - 1]}</span>
          </div>
        </div>
      </div>
    </div>
  )
}

export function ProjectAnalytics({
  projects,
  activity,
}: Readonly<{ projects: ProjectStat[]; activity: ProjectDayActivity[] }>) {
  if (projects.length === 0) return null

  const charted = new Set(activity.map(a => a.project))
  const hidden = projects.length - charted.size

  return (
    <div className="space-y-5">
      <ChartCard
        title="Projects"
        subtitle="Sessions, conversation tokens and spend per project, ranked by cost. Costs are the same stored per-session figures the totals above use."
      >
        <ProjectTable projects={projects} />
      </ChartCard>

      {activity.length > 0 && (
        <ChartCard
          title="What I Worked On, When"
          subtitle={
            hidden > 0
              ? `Daily spend per project, darker is more. The ${hidden} least expensive project${hidden === 1 ? '' : 's'} are listed in the table above but not charted here.`
              : 'Daily spend per project, darker is more.'
          }
        >
          <ProjectDayStrip activity={activity} projects={projects} />
        </ChartCard>
      )}
    </div>
  )
}
