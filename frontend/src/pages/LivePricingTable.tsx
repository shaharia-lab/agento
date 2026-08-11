/**
 * The rates the costs on this page were actually computed with.
 *
 * This replaces a table of Anthropic prices hardcoded in February 2026, which
 * had drifted from the editable catalog behind Settings → Model Pricing and
 * stated that "unknown models fall back to Sonnet pricing" — the exact opposite
 * of what the backend does. Unmatched models are disclosed in an unpriced
 * bucket and contribute no cost, deliberately, so that text documented away a
 * correctness feature. Reading the catalog live means the table cannot drift
 * again: it is the same data the resolver prices with.
 */
import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { ChevronDown } from 'lucide-react'

import { pricingApi } from '@/lib/api'
import { formatRateDate } from '@/lib/pricing'
import type { PricedModel } from '@/types'

/** Formats a per-MTok rate, or an em dash when a model is non-billable. */
function rate(value: number, billable: boolean): string {
  if (!billable) return '—'
  return `$${value.toLocaleString(undefined, { maximumFractionDigits: 2 })}`
}

/**
 * Orders models the way a reader scans them: by provider, then by price, so the
 * expensive models a reader is looking for are at the top of their group.
 */
function sortForDisplay(models: PricedModel[]): PricedModel[] {
  return [...models].sort((a, b) => {
    if (a.provider !== b.provider) return a.provider.localeCompare(b.provider)
    return (b.current?.output_per_mtok ?? 0) - (a.current?.output_per_mtok ?? 0)
  })
}

export function LivePricingTable() {
  const [open, setOpen] = useState(false)
  const [models, setModels] = useState<PricedModel[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  // Fetched on first expand rather than on mount: the catalog is a detail view
  // behind a disclosure, and every analytics page render should not pay for it.
  useEffect(() => {
    if (!open || models || error) return
    pricingApi
      .catalog()
      .then(catalog => setModels(catalog.models))
      .catch(err => setError(err instanceof Error ? err.message : 'Failed to load pricing catalog'))
  }, [open, models, error])

  const priced = models ? sortForDisplay(models.filter(m => m.current !== null)) : []

  return (
    <div className="rounded-md border border-zinc-200 dark:border-zinc-700/50 bg-zinc-50 dark:bg-zinc-800/40 mb-4">
      <button
        onClick={() => setOpen(o => !o)}
        className="flex w-full items-center justify-between px-3 py-2 text-xs font-medium text-zinc-600 dark:text-zinc-300 cursor-pointer"
      >
        <span>Rates these costs were computed with</span>
        <ChevronDown
          className={`h-3.5 w-3.5 shrink-0 transition-transform duration-200 ${open ? 'rotate-180' : ''}`}
        />
      </button>
      {open && (
        <div className="px-3 pb-2.5 border-t border-zinc-200 dark:border-zinc-700/50 pt-2">
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-2">
            Each message is priced at its own model and timestamp against this catalog, so a session
            that spans a price change keeps the rate in force when its tokens were spent. A model
            with no rate here contributes no cost and is reported separately. Costs are never
            guessed from another model&apos;s price. Edit rates in{' '}
            <Link to="/settings" className="underline hover:text-zinc-900 dark:hover:text-zinc-100">
              Settings → Model Pricing
            </Link>
            .
          </p>
          {error && <p className="text-xs text-red-600 dark:text-red-400">{error}</p>}
          {!models && !error && <p className="text-xs text-zinc-400">Loading rates…</p>}
          {models && (
            <div className="max-h-72 overflow-y-auto">
              <table className="w-full text-[12px] text-zinc-600 dark:text-zinc-300 border-collapse">
                <thead className="sticky top-0 bg-zinc-50 dark:bg-zinc-800">
                  <tr className="border-b border-zinc-200 dark:border-zinc-700/50">
                    <th className="text-left font-medium pb-1 pr-4">Model</th>
                    <th className="text-right font-medium pb-1 pr-4">Input</th>
                    <th className="text-right font-medium pb-1 pr-4">Output</th>
                    <th className="text-right font-medium pb-1 pr-4">Cache write 5m</th>
                    <th className="text-right font-medium pb-1 pr-4">Cache read</th>
                    <th className="text-right font-medium pb-1">Since</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-zinc-100 dark:divide-zinc-700/30">
                  {priced.map(model => {
                    const r = model.current!
                    return (
                      <tr key={`${model.provider}-${model.model_pattern}`}>
                        <td className="py-0.5 pr-4">
                          <span className="font-medium">{model.display_name}</span>
                          <span className="text-zinc-400 dark:text-zinc-500">
                            {' '}
                            · {model.provider}
                          </span>
                          {r.estimated && (
                            <span
                              className="ml-1 text-amber-600 dark:text-amber-400"
                              title="Best-effort rate: this pattern names a model family rather than a concrete model"
                            >
                              est.
                            </span>
                          )}
                        </td>
                        <td className="py-0.5 pr-4 text-right tabular-nums">
                          {rate(r.input_per_mtok, r.billable)}
                        </td>
                        <td className="py-0.5 pr-4 text-right tabular-nums">
                          {rate(r.output_per_mtok, r.billable)}
                        </td>
                        <td className="py-0.5 pr-4 text-right tabular-nums">
                          {rate(r.cache_write_5m_per_mtok, r.billable)}
                        </td>
                        <td className="py-0.5 pr-4 text-right tabular-nums">
                          {rate(r.cache_read_per_mtok, r.billable)}
                        </td>
                        <td className="py-0.5 text-right text-zinc-400 dark:text-zinc-500">
                          {formatRateDate(r.effective_from)}
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
              <p className="pt-1.5 text-zinc-400 dark:text-zinc-500 italic text-[12px]">
                All rates are per million tokens (MTok). Cache writes with a 1-hour TTL bill higher
                than the 5-minute column shown here.
              </p>
            </div>
          )}
        </div>
      )}
    </div>
  )
}
