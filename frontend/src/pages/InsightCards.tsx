/**
 * Actionable insight cards — what replaced the Overall Productivity Score.
 *
 * The score was `0.5·avg(autonomy) + 0.3·avg(cache-hit)·100 + 0.2·error-free%`:
 * three unweighted per-session averages, arbitrary weights, and one component
 * (error-free rate) that counted a grep matching nothing as a failure. A user
 * reading "58 / 100 · Moderate" learned nothing they could act on.
 *
 * Each card states one fact with a number and, where there is one, the thing to
 * do about it. The numbers come from the backend; only the phrasing lives here.
 */
import { Coins, Layers, Bot, Flame } from 'lucide-react'

import { formatCost, formatDuration, formatTokens } from '@/lib/format'
import type { InsightCard } from '@/types'

import { formatModelName } from './analyticsShared'

interface CardCopy {
  icon: React.ElementType
  headline: string
  fact: string
  action?: string
}

/** Phrases one card. Returns null for a kind this build does not know. */
function copyFor(card: InsightCard): CardCopy | null {
  switch (card.kind) {
    case 'cache_savings':
      // Framed against what the window actually cost: "$102,623 saved" alone
      // invites the reading that it was earned, when the point is that the bill
      // would have been six times larger.
      return {
        icon: Coins,
        headline: `Caching kept the bill at ${formatCost(card.comparison_usd ?? 0)}`,
        fact: `Without it, the ${formatTokens(card.tokens ?? 0)} tokens served from cache would have been billed as fresh input — about ${formatCost((card.comparison_usd ?? 0) + (card.amount_usd ?? 0))} for the same work.`,
        action:
          'Longer-lived sessions keep this working; frequent restarts pay the full price again.',
      }
    case 'model_low_cache':
      return {
        icon: Layers,
        headline: `${formatModelName(card.model ?? '')} is barely served from cache`,
        fact: `Only ${(card.percent ?? 0).toFixed(1)}% of its input came from cache, so ${formatTokens(card.tokens ?? 0)} tokens of context were re-billed as fresh input for ${formatCost(card.amount_usd ?? 0)}.`,
        action:
          'It will dominate token-volume charts and barely register on cost charts — judge it by cost.',
      }
    case 'delegation_mix':
      return {
        icon: Bot,
        headline: `${(card.percent ?? 0).toFixed(1)}% of spend was delegated`,
        fact: `${formatCost(card.amount_usd ?? 0)} across ${card.count ?? 0} session${card.count === 1 ? '' : 's'} ran in sub-agents${card.model ? `, mostly on ${formatModelName(card.model)}` : ''}.`,
        action:
          'Delegating to a cheaper model is the lever here — this is the number that shows whether it is working.',
      }
    case 'expensive_sessions':
      return {
        icon: Flame,
        headline: `Top ${card.count ?? 0} sessions are ${(card.percent ?? 0).toFixed(1)}% of the bill`,
        fact: `They cost ${formatCost(card.amount_usd ?? 0)} together and worked ${formatDuration(card.avg_duration_ms ?? 0)} on average (active time).`,
        action:
          'A few long runs dominate the total; splitting them is where a change of habit pays.',
      }
    default:
      return null
  }
}

export function InsightCardGrid({ cards }: Readonly<{ cards: InsightCard[] }>) {
  const rendered = cards.map(card => ({ card, copy: copyFor(card) })).filter(c => c.copy !== null)

  // No cards is a legitimate state — a window with little activity has nothing
  // specific to say — and an empty grid says it better than a placeholder.
  if (rendered.length === 0) return null

  return (
    <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
      {rendered.map(({ card, copy }) => {
        const Icon = copy!.icon
        return (
          <div
            key={card.kind}
            className="rounded-lg border border-zinc-200 dark:border-zinc-700/50 bg-white dark:bg-zinc-900 p-4"
          >
            <div className="flex items-start gap-3">
              <span className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-zinc-100 dark:bg-zinc-800">
                <Icon className="h-3.5 w-3.5 text-zinc-500 dark:text-zinc-400" />
              </span>
              <div className="min-w-0">
                <p className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
                  {copy!.headline}
                </p>
                <p className="text-xs text-zinc-600 dark:text-zinc-400 mt-1">{copy!.fact}</p>
                {copy!.action && (
                  <p className="text-xs text-zinc-400 dark:text-zinc-500 mt-1.5">{copy!.action}</p>
                )}
              </div>
            </div>
          </div>
        )
      })}
    </div>
  )
}
