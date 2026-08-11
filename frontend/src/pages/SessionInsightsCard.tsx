/**
 * Per-session insights on the session detail page.
 *
 * `GET /api/claude-sessions/{id}/insights` had no caller anywhere in the app:
 * turns, steps per turn, response times, the longest autonomous chain, tool
 * errors and the session type were all computed by the insight pipeline, stored
 * in SQLite and typed in TypeScript, and never reached a pixel. This renders
 * them.
 *
 * Two deliberate omissions. The autonomy score is not shown — it is
 * `100 · (1/turns) · min(1, log10(steps/turn + 1))`, which caps a two-turn
 * session at 50 however autonomous it was and pushes a long collaborative one
 * toward zero, so it does not measure what a reader would take it to mean.
 * `has_errors` is not shown as a verdict either: a grep that matched nothing
 * sets it, and flagging a whole session for that is what made 63% of sessions
 * look failed. The error *rate* is shown instead, unstyled.
 */
import { useEffect, useState } from 'react'

import { insightsApi } from '@/lib/api'
import { formatDuration } from '@/lib/format'
import type { SessionInsight } from '@/types'

/** Rounds to at most one decimal, dropping a trailing ".0". */
function num(value: number): string {
  return Number.isInteger(value) ? String(value) : value.toFixed(1)
}

/** A response time is only meaningful once there is a turn to measure. */
function responseTime(ms: number): string | null {
  if (ms <= 0) return null
  return formatDuration(Math.round(ms))
}

interface Metric {
  label: string
  value: string
  hint?: string
}

function metricsFor(insight: SessionInsight): Metric[] {
  const metrics: Metric[] = [
    { label: 'Turns', value: String(insight.turn_count), hint: 'genuine human prompts' },
    {
      label: 'Steps / turn',
      value: num(insight.steps_per_turn_avg),
      // Sub-agent transcripts run through the same processors, so their steps
      // count while their events never open a turn. On a heavily delegating
      // session that makes this number large; saying why beats leaving a reader
      // to conclude the metric is broken.
      hint: 'delegated steps included',
    },
    { label: 'Tool calls', value: String(insight.tool_calls_total), hint: 'sub-agents included' },
    {
      label: 'Longest autonomous chain',
      value: String(insight.longest_autonomous_chain),
      hint: 'steps without a human turn',
    },
    {
      label: 'Max consecutive tool calls',
      value: String(insight.max_consecutive_tool_calls),
    },
    {
      label: 'Active duration',
      value: insight.active_duration_ms > 0 ? formatDuration(insight.active_duration_ms) : '—',
      hint: 'idle gaps over 10 min excluded',
    },
    {
      label: 'Claude working time',
      value:
        insight.claude_working_time_ms > 0 ? formatDuration(insight.claude_working_time_ms) : '—',
      hint: 'time spent producing responses',
    },
  ]

  const userReply = responseTime(insight.avg_user_response_time_ms)
  if (userReply) metrics.push({ label: 'Avg your reply', value: userReply })

  const claudeReply = responseTime(insight.avg_claude_response_time_ms)
  if (claudeReply) metrics.push({ label: 'Avg Claude reply', value: claudeReply })

  if (insight.tool_calls_total > 0) {
    metrics.push({
      label: 'Tool errors',
      value: `${insight.tool_error_count} · ${(insight.tool_error_rate * 100).toFixed(1)}%`,
      hint: 'a failing grep counts',
    })
  }

  return metrics
}

export function SessionInsightsCard({ sessionId }: Readonly<{ sessionId: string }>) {
  const [insight, setInsight] = useState<SessionInsight | null>(null)
  const [failed, setFailed] = useState(false)

  useEffect(() => {
    let cancelled = false
    insightsApi
      .getSession(sessionId)
      .then(result => {
        if (!cancelled) setInsight(result)
      })
      .catch(() => {
        if (!cancelled) setFailed(true)
      })
    return () => {
      cancelled = true
    }
  }, [sessionId])

  // A session the pipeline has not reached yet is an ordinary state — it
  // processes in the background — so the card simply does not appear rather
  // than occupying the sidebar with an error or a spinner.
  if (failed || !insight) return null

  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700/50 bg-white dark:bg-zinc-900 p-3.5">
      <div className="flex items-baseline justify-between mb-2.5">
        <h3 className="text-[13px] font-semibold text-zinc-900 dark:text-zinc-100">Insights</h3>
        {insight.session_type && (
          <span className="text-[11px] text-zinc-400 dark:text-zinc-500">
            {insight.session_type}
          </span>
        )}
      </div>
      <dl className="grid grid-cols-2 gap-x-3 gap-y-2.5">
        {metricsFor(insight).map(metric => (
          <div key={metric.label}>
            <dt className="text-[11px] text-zinc-500 dark:text-zinc-400">{metric.label}</dt>
            <dd className="text-[13px] font-medium tabular-nums text-zinc-900 dark:text-zinc-100">
              {metric.value}
            </dd>
            {metric.hint && (
              <dd className="text-[10px] text-zinc-400 dark:text-zinc-500">{metric.hint}</dd>
            )}
          </div>
        ))}
      </dl>
    </div>
  )
}
