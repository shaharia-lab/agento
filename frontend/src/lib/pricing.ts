import type { PricedModel, PricingRate, PricingRateInput } from '@/types'

/**
 * Formats an RFC3339 or YYYY-MM-DD instant as a readable date: "9 Aug 2026".
 *
 * Rendered in UTC on purpose. A rate is keyed to a day and stored as midnight
 * UTC, so formatting it in the viewer's timezone shows the previous day to
 * anyone west of UTC — and would make the add-form promise a window the server
 * will not honour. It also keeps this in step with toDateInputValue, which
 * reads the same value back through toISOString.
 */
export function formatRateDate(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleDateString(undefined, {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
    timeZone: 'UTC',
  })
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

/** True when the rate prices by context length rather than at one flat rate. */
export function isTiered(rate: Pick<PricingRate, 'tiers'>): boolean {
  return (rate.tiers?.length ?? 0) > 0
}

/**
 * Renders a token bound the way the provider's pricing page writes it — 256000
 * as "256K", 1000000 as "1M" — falling back to a grouped number for a bound
 * that is not a round multiple.
 */
export function formatTokenBound(tokens: number): string {
  if (tokens >= 1_000_000 && tokens % 1_000_000 === 0) return `${tokens / 1_000_000}M`
  if (tokens >= 1_000 && tokens % 1_000 === 0) return `${tokens / 1_000}K`
  return tokens.toLocaleString('en-US')
}

/**
 * Describes each context-length band as "range: $in/$out".
 *
 * The last band is written as an open range ("> 256K") rather than closed at
 * its declared bound, because that is what actually happens: a request larger
 * than every bound bills at the highest band.
 */
export function describeTierBands(rate: Pick<PricingRate, 'tiers'>): string[] {
  const tiers = rate.tiers ?? []
  return tiers.map((t, i) => {
    const lower = i === 0 ? 0 : tiers[i - 1].max_input_tokens
    const last = i === tiers.length - 1
    let range: string
    if (last) {
      range = lower === 0 ? 'any size' : `> ${formatTokenBound(lower)}`
    } else if (lower === 0) {
      range = `≤ ${formatTokenBound(t.max_input_tokens)}`
    } else {
      range = `${formatTokenBound(lower)}–${formatTokenBound(t.max_input_tokens)}`
    }
    return `${range}: $${t.input_per_mtok}/$${t.output_per_mtok}`
  })
}

/**
 * The input/output prices to show for a rate at a glance. A tiered rate spans
 * a range, and showing only its lowest band would present the cheapest price
 * as if it were the whole price — the exact misreading #218 exists to end.
 */
export function summarizeRatePrices(rate: PricingRate): string {
  const tiers = rate.tiers ?? []
  if (tiers.length === 0) return `$${rate.input_per_mtok} / $${rate.output_per_mtok}`
  const ins = tiers.map(t => t.input_per_mtok)
  const outs = tiers.map(t => t.output_per_mtok)
  const lo = `$${Math.min(...ins)} / $${Math.min(...outs)}`
  const hi = `$${Math.max(...ins)} / $${Math.max(...outs)}`
  return lo === hi ? lo : `${lo} – ${hi}`
}
