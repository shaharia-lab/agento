import type { ClaudeSessionSummary } from '../types'
import { sessionCost, sessionTokens } from './sessionMetrics'

/**
 * One day's worth of sessions plus the roll-ups the day header shows.
 *
 * Days are keyed by the *local* calendar date of `last_activity`, matching the
 * rest of the analytics surface (#190): a day is meaningless until you say
 * whose it is, and the list is read by the person whose machine ran it.
 */
export interface SessionDayGroup {
  /** Local `YYYY-MM-DD` of the day, stable across renders. */
  key: string
  /** "Today", "Yesterday", or e.g. "Fri 7 Aug" (year appended when not this one). */
  label: string
  sessions: ClaudeSessionSummary[]
  messageCount: number
  /** Input + output tokens, main thread plus delegated sub-agents. */
  tokens: number
  /** Main-thread plus sub-agent cost in USD. */
  cost: number
}

function dayKey(d: Date): string {
  const m = String(d.getMonth() + 1).padStart(2, '0')
  const day = String(d.getDate()).padStart(2, '0')
  return `${d.getFullYear()}-${m}-${day}`
}

/**
 * Human label for a day header. Relative for the two days people actually
 * recognise, absolute after that — "3d ago" as a *heading* forces arithmetic
 * the row-level relative times already cover.
 */
export function dayLabel(d: Date, now: Date): string {
  const key = dayKey(d)
  if (key === dayKey(now)) return 'Today'
  const yesterday = new Date(now)
  yesterday.setDate(yesterday.getDate() - 1)
  if (key === dayKey(yesterday)) return 'Yesterday'

  const weekday = d.toLocaleDateString(undefined, { weekday: 'short' })
  const month = d.toLocaleDateString(undefined, { month: 'short' })
  const base = `${weekday} ${d.getDate()} ${month}`
  return d.getFullYear() === now.getFullYear() ? base : `${base} ${d.getFullYear()}`
}

/**
 * Groups sessions into newest-first day buckets with per-day roll-ups.
 *
 * Grouping stays client-side even though filtering and paging moved to SQL. It
 * operates on the pages loaded so far, which is bounded, and it is the only
 * arrangement under which a day header's roll-up is exactly the sum of the rows
 * beneath it. A server-side `GROUP BY date(last_activity)` would report the
 * whole day's totals above however many of its rows had been paged in, so the
 * header and its rows would disagree on every day split across a page boundary.
 *
 * Sorting happens here rather than relying on the API order: the day headers
 * carry totals, and a single out-of-order row would silently split one day into
 * two buckets with half the roll-up each.
 */
export function groupSessionsByDay(
  sessions: readonly ClaudeSessionSummary[],
  now: Date = new Date(),
): SessionDayGroup[] {
  const ordered = [...sessions].sort(
    (a, b) => new Date(b.last_activity).getTime() - new Date(a.last_activity).getTime(),
  )

  const groups: SessionDayGroup[] = []
  const byKey = new Map<string, SessionDayGroup>()

  for (const s of ordered) {
    const d = new Date(s.last_activity)
    // An unparseable timestamp still belongs somewhere visible; bucketing it
    // under "Unknown" beats dropping the row out of the list entirely.
    const valid = !Number.isNaN(d.getTime())
    const key = valid ? dayKey(d) : 'unknown'
    let group = byKey.get(key)
    if (!group) {
      group = {
        key,
        label: valid ? dayLabel(d, now) : 'Unknown date',
        sessions: [],
        messageCount: 0,
        tokens: 0,
        cost: 0,
      }
      byKey.set(key, group)
      groups.push(group)
    }
    group.sessions.push(s)
    group.messageCount += s.message_count ?? 0
    group.tokens += sessionTokens(s)
    group.cost += sessionCost(s)
  }

  return groups
}
