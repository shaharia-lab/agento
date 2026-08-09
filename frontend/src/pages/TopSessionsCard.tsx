/**
 * Session leaderboards.
 *
 * "Which sessions cost me the most / ran the longest / burned the most tokens"
 * had no answer anywhere: the sessions list can filter but never ranked, so the
 * outlier a user is hunting for was findable only by scrolling. Each row links
 * straight to the session.
 *
 * The three boards are separate rather than one sortable table because they
 * genuinely pick out different sessions — the most expensive session on this
 * corpus is not the longest — and seeing which is which is the point.
 */
import { useState } from 'react'
import { Link } from 'react-router-dom'

import { formatCost, formatDuration, formatTokens } from '@/lib/format'
import type { SessionRanking, TopSessions } from '@/types'

import { ChartCard, formatModelName } from './analyticsShared'
import { shortProject } from './ProjectAnalytics'

type Board = 'cost' | 'duration' | 'tokens'

const BOARDS: { key: Board; label: string; measure: (r: SessionRanking) => string }[] = [
  { key: 'cost', label: 'Most expensive', measure: r => formatCost(r.cost_usd) },
  { key: 'duration', label: 'Longest', measure: r => formatDuration(r.duration_ms) },
  { key: 'tokens', label: 'Most tokens', measure: r => formatTokens(r.tokens) },
]

function rowsFor(top: TopSessions, board: Board): SessionRanking[] {
  if (board === 'duration') return top.by_duration
  if (board === 'tokens') return top.by_tokens
  return top.by_cost
}

export function TopSessionsCard({ top }: Readonly<{ top: TopSessions }>) {
  const [board, setBoard] = useState<Board>('cost')
  const rows = rowsFor(top, board)
  const measure = BOARDS.find(b => b.key === board)!.measure

  if (top.by_cost.length === 0 && top.by_duration.length === 0 && top.by_tokens.length === 0) {
    return null
  }

  return (
    <ChartCard title="Top Sessions">
      <div className="flex gap-1 mb-3">
        {BOARDS.map(b => (
          <button
            key={b.key}
            onClick={() => setBoard(b.key)}
            className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors ${
              board === b.key
                ? 'bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900'
                : 'bg-zinc-100 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-400 hover:bg-zinc-200 dark:hover:bg-zinc-700'
            }`}
          >
            {b.label}
          </button>
        ))}
      </div>

      {rows.length === 0 ? (
        <p className="text-sm text-zinc-400 dark:text-zinc-500 py-6 text-center">
          Nothing to rank in this range.
        </p>
      ) : (
        <ol className="space-y-1">
          {rows.map((r, i) => (
            <li key={r.session_id}>
              <Link
                to={`/claude-sessions/${r.session_id}`}
                className="flex items-center gap-3 rounded-md px-2 py-1.5 hover:bg-zinc-50 dark:hover:bg-zinc-800/50 transition-colors"
              >
                <span className="w-4 shrink-0 text-right text-[11px] tabular-nums text-zinc-400 dark:text-zinc-500">
                  {i + 1}
                </span>
                <span className="flex-1 min-w-0">
                  <span className="block truncate text-xs text-zinc-800 dark:text-zinc-200">
                    {r.title || r.session_id}
                  </span>
                  <span className="block truncate text-[11px] text-zinc-400 dark:text-zinc-500">
                    <span className="font-mono">{shortProject(r.project)}</span>
                    {' · '}
                    {formatModelName(r.model)}
                    {r.subagent_count > 0 && ` · ${r.subagent_count} sub-agents`}
                  </span>
                </span>
                <span className="shrink-0 text-xs font-medium tabular-nums text-zinc-900 dark:text-zinc-100">
                  {measure(r)}
                </span>
              </Link>
            </li>
          ))}
        </ol>
      )}
    </ChartCard>
  )
}
