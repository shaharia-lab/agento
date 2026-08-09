import type { PricedModel, PricingRate, PricingRateInput } from '@/types'

/** Formats an RFC3339 or YYYY-MM-DD instant as a readable date: "9 Aug 2026". */
export function formatRateDate(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' })
}

/** Converts an RFC3339 instant to the YYYY-MM-DD an `<input type="date">` expects. */
export function toDateInputValue(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  return d.toISOString().slice(0, 10)
}

/** Today as YYYY-MM-DD, the sensible default for a new rate. */
export function todayInputValue(now: Date = new Date()): string {
  return new Date(now.getTime() - now.getTimezoneOffset() * 60_000).toISOString().slice(0, 10)
}

/**
 * Describes, in plain language, which usage a rate effective from `from` will
 * govern given the rates a model already has.
 *
 * This exists because appending and correcting are easy to confuse, and the
 * consequence of each is invisible until costs move. Stating the window before
 * submission is what makes "the price changed" the obvious action and "I typed
 * it wrong" the deliberate one.
 */
export function describeRateWindow(existing: PricingRate[], from: string): string {
  const start = new Date(from)
  if (Number.isNaN(start.getTime())) return 'Enter a valid date to see which usage this will cover.'

  const later = existing
    .filter(r => new Date(r.effective_from) > start)
    .sort((a, b) => new Date(a.effective_from).getTime() - new Date(b.effective_from).getTime())

  const earlier = existing.filter(r => new Date(r.effective_from) <= start)
  const tail =
    earlier.length > 0
      ? ` Earlier sessions keep the rate they were costed at.`
      : ` No earlier rate exists, so sessions before this date stay unpriced.`

  if (later.length > 0) {
    return (
      `Sessions from ${formatRateDate(from)} until ${formatRateDate(later[0].effective_from)}` +
      ` will use this rate.${tail}`
    )
  }
  return `Sessions from ${formatRateDate(from)} onward will use this rate.${tail}`
}

/**
 * True when a rate already exists for this exact date — the case where the user
 * means "correct", not "add". The server rejects it too; catching it in the
 * form turns a 409 into a question.
 */
export function hasRateOn(existing: PricingRate[], from: string): boolean {
  const target = toDateInputValue(from)
  return existing.some(r => toDateInputValue(r.effective_from) === target)
}

/** A blank rate form for a model, or for a brand-new pattern. */
export function emptyRateInput(modelPattern = '', provider = ''): PricingRateInput {
  return {
    provider,
    model_pattern: modelPattern,
    match_type: 'prefix',
    display_name: '',
    input_per_mtok: 0,
    output_per_mtok: 0,
    cache_write_5m_per_mtok: 0,
    cache_write_1h_per_mtok: 0,
    cache_read_per_mtok: 0,
    effective_from: todayInputValue(),
    source: '',
    billable: true,
    estimated: false,
  }
}

/** Pre-fills a form from an existing rate, for correcting it in place. */
export function rateToInput(r: PricingRate): PricingRateInput {
  return {
    provider: r.provider,
    model_pattern: r.model_pattern,
    match_type: r.match_type,
    display_name: r.display_name,
    input_per_mtok: r.input_per_mtok,
    output_per_mtok: r.output_per_mtok,
    cache_write_5m_per_mtok: r.cache_write_5m_per_mtok,
    cache_write_1h_per_mtok: r.cache_write_1h_per_mtok,
    cache_read_per_mtok: r.cache_read_per_mtok,
    effective_from: toDateInputValue(r.effective_from),
    source: r.source,
    billable: r.billable,
    estimated: r.estimated,
  }
}

/**
 * Client-side mirror of the server's coherence rule, so the form can explain
 * the problem instead of surfacing a 422. Returns null when valid.
 */
export function validateRateInput(input: PricingRateInput): string | null {
  if (!input.model_pattern.trim()) return 'Model pattern is required.'
  if (!input.effective_from) return 'Effective date is required.'

  const rates = [
    input.input_per_mtok,
    input.output_per_mtok,
    input.cache_write_5m_per_mtok,
    input.cache_write_1h_per_mtok,
    input.cache_read_per_mtok,
  ]
  if (rates.some(v => v < 0 || Number.isNaN(v))) return 'Rates must be zero or a positive number.'

  if (input.billable && (input.input_per_mtok <= 0 || input.output_per_mtok <= 0)) {
    return 'A billable model needs a positive input and output rate.'
  }
  if (!input.billable && rates.some(v => v !== 0)) {
    return 'A non-billable model must have every rate set to zero.'
  }
  return null
}

/** Groups models by provider for display, providers sorted, unnamed last. */
export function groupByProvider(models: PricedModel[]): [string, PricedModel[]][] {
  const groups = new Map<string, PricedModel[]>()
  for (const m of models) {
    const key = m.provider || 'other'
    const list = groups.get(key)
    if (list) list.push(m)
    else groups.set(key, [m])
  }
  return [...groups.entries()].sort(([a], [b]) => {
    if (a === 'other') return 1
    if (b === 'other') return -1
    return a.localeCompare(b)
  })
}
