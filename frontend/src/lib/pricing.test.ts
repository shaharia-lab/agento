import { describe, it, expect } from 'vitest'
import type { PricedModel, PricingRate } from '@/types'
import {
  describeRateWindow,
  describeTierBands,
  formatRateDate,
  formatTokenBound,
  emptyRateInput,
  groupByProvider,
  hasRateOn,
  isTiered,
  rateToInput,
  summarizeRatePrices,
  toDateInputValue,
  validateRateInput,
} from './pricing'

function rate(effectiveFrom: string, overrides: Partial<PricingRate> = {}): PricingRate {
  return {
    id: 1,
    provider: 'anthropic',
    model_pattern: 'claude-opus-5',
    match_type: 'prefix',
    display_name: 'Claude Opus 5',
    input_per_mtok: 5,
    output_per_mtok: 25,
    cache_write_5m_per_mtok: 6.25,
    cache_write_1h_per_mtok: 10,
    cache_read_per_mtok: 0.5,
    effective_from: effectiveFrom,
    source: 'test',
    is_builtin: true,
    user_modified: false,
    billable: true,
    estimated: false,
    ...overrides,
  }
}

describe('describeRateWindow', () => {
  // The whole point of effective dating is that adding a rate does not rewrite
  // history. The form has to say so before the user commits.
  it('states an open-ended window when nothing later exists', () => {
    const got = describeRateWindow([rate('2026-01-01T00:00:00Z')], '2026-06-01')
    expect(got).toContain('onward')
    expect(got).toContain('Earlier sessions keep the rate they were costed at.')
  })

  it('bounds the window when a later rate already exists', () => {
    const existing = [rate('2026-01-01T00:00:00Z'), rate('2026-09-01T00:00:00Z')]
    const got = describeRateWindow(existing, '2026-06-01')
    expect(got).toContain('until')
    expect(got).toContain('Sep 1, 2026')
  })

  it('warns when there is no earlier rate to fall back on', () => {
    expect(describeRateWindow([], '2026-06-01')).toContain('stay unpriced')
  })

  it('refuses to guess from an unparseable date', () => {
    expect(describeRateWindow([], 'not a date')).toContain('valid date')
  })
})

describe('hasRateOn', () => {
  it('detects a collision on the same day, which means correct rather than add', () => {
    const existing = [rate('2026-06-01T00:00:00Z')]
    expect(hasRateOn(existing, '2026-06-01')).toBe(true)
    expect(hasRateOn(existing, '2026-06-02')).toBe(false)
  })
})

describe('validateRateInput', () => {
  it('accepts a well-formed billable rate', () => {
    const input = { ...emptyRateInput('k3'), input_per_mtok: 3, output_per_mtok: 15 }
    expect(validateRateInput(input)).toBeNull()
  })

  it('rejects a billable model with no rates — the invisible $0 bug', () => {
    expect(validateRateInput(emptyRateInput('k3'))).toContain('positive input and output')
  })

  it('rejects a non-billable model that still carries a price', () => {
    const input = { ...emptyRateInput('k3'), billable: false, input_per_mtok: 3 }
    expect(validateRateInput(input)).toContain('every rate set to zero')
  })

  it('accepts a non-billable model priced at zero throughout', () => {
    expect(validateRateInput({ ...emptyRateInput('embed/'), billable: false })).toBeNull()
  })

  it('rejects negative rates and a missing pattern', () => {
    expect(validateRateInput({ ...emptyRateInput(''), input_per_mtok: 1 })).toContain('required')
    const negative = { ...emptyRateInput('k3'), input_per_mtok: -1, output_per_mtok: 5 }
    expect(validateRateInput(negative)).toContain('positive number')
  })
})

describe('rateToInput', () => {
  it('round-trips a rate into a form, converting the date for the date input', () => {
    const got = rateToInput(rate('2026-06-01T00:00:00Z', { user_modified: true }))
    expect(got.effective_from).toBe('2026-06-01')
    expect(got.input_per_mtok).toBe(5)
    expect(got.billable).toBe(true)
  })
})

describe('toDateInputValue', () => {
  it('returns empty for an unparseable value rather than throwing', () => {
    expect(toDateInputValue('nonsense')).toBe('')
  })
})

describe('formatRateDate', () => {
  // A rate is keyed to a day and stored as midnight UTC. Rendering it in local
  // time shows the previous day to anyone west of UTC, which would make the
  // history read shifted and the add-form promise a window the server will not
  // honour. Pinning to UTC also keeps it in step with toDateInputValue.
  it('renders the stored UTC day regardless of the viewer timezone', () => {
    expect(formatRateDate('2026-06-01T00:00:00Z')).toContain('2026')
    expect(formatRateDate('2026-06-01T00:00:00Z')).toContain('Jun')
    expect(formatRateDate('2026-06-01T00:00:00Z')).toContain('1')
    expect(formatRateDate('2026-06-01T00:00:00Z')).not.toContain('May')
  })

  it('agrees with the value the date input round-trips', () => {
    const iso = '2026-06-01T00:00:00Z'
    expect(toDateInputValue(iso)).toBe('2026-06-01')
    expect(formatRateDate(toDateInputValue(iso))).toBe(formatRateDate(iso))
  })

  it('passes an unparseable value through untouched', () => {
    expect(formatRateDate('nonsense')).toBe('nonsense')
  })
})

describe('groupByProvider', () => {
  it('sorts providers alphabetically and sinks unnamed ones to the end', () => {
    const models = [
      { provider: 'moonshot' },
      { provider: '' },
      { provider: 'anthropic' },
    ] as PricedModel[]
    expect(groupByProvider(models).map(([p]) => p)).toEqual(['anthropic', 'moonshot', 'other'])
  })
})

// The seeded qwen3.6-flash shape: base band plus the 256K-1M band that #218
// added, the pair with the widest gap in the catalog.
function tieredRate(): PricingRate {
  return rate('2020-01-01T00:00:00Z', {
    provider: 'alibaba',
    model_pattern: 'qwen3.6-flash',
    display_name: 'Qwen3.6 Flash',
    input_per_mtok: 0.25,
    output_per_mtok: 1.5,
    cache_write_5m_per_mtok: 0.3125,
    cache_write_1h_per_mtok: 0.3125,
    cache_read_per_mtok: 0.025,
    tiers: [
      {
        max_input_tokens: 256000,
        input_per_mtok: 0.25,
        output_per_mtok: 1.5,
        cache_write_5m_per_mtok: 0.3125,
        cache_write_1h_per_mtok: 0.3125,
        cache_read_per_mtok: 0.025,
      },
      {
        max_input_tokens: 1000000,
        input_per_mtok: 1,
        output_per_mtok: 4,
        cache_write_5m_per_mtok: 1.25,
        cache_write_1h_per_mtok: 1.25,
        cache_read_per_mtok: 0.1,
      },
    ],
  })
}

describe('isTiered', () => {
  it('is false for a flat rate and for an empty band list', () => {
    expect(isTiered(rate('2020-01-01T00:00:00Z'))).toBe(false)
    expect(isTiered({ tiers: [] })).toBe(false)
  })

  it('is true once the rate declares a band', () => {
    expect(isTiered(tieredRate())).toBe(true)
  })
})

describe('formatTokenBound', () => {
  it('writes bounds the way the provider page does', () => {
    expect(formatTokenBound(32000)).toBe('32K')
    expect(formatTokenBound(256000)).toBe('256K')
    expect(formatTokenBound(1000000)).toBe('1M')
  })

  it('falls back to a grouped number for a bound that is not round', () => {
    expect(formatTokenBound(262144)).toBe('262,144')
    expect(formatTokenBound(500)).toBe('500')
  })
})

describe('describeTierBands', () => {
  it('describes each band, leaving the last one open-ended', () => {
    // The top band is written "> 256K" rather than "256K-1M" because that is
    // what is charged: a request above every bound bills at the highest band.
    expect(describeTierBands(tieredRate())).toEqual(['≤ 256K: $0.25/$1.5', '> 256K: $1/$4'])
  })

  it('names the interior range of a three-band model', () => {
    const r = rate('2020-01-01T00:00:00Z', {
      tiers: [
        {
          max_input_tokens: 32000,
          input_per_mtok: 1.2,
          output_per_mtok: 6,
          cache_write_5m_per_mtok: 1.5,
          cache_write_1h_per_mtok: 1.5,
          cache_read_per_mtok: 0.12,
        },
        {
          max_input_tokens: 128000,
          input_per_mtok: 2.4,
          output_per_mtok: 12,
          cache_write_5m_per_mtok: 3,
          cache_write_1h_per_mtok: 3,
          cache_read_per_mtok: 0.24,
        },
        {
          max_input_tokens: 256000,
          input_per_mtok: 3,
          output_per_mtok: 15,
          cache_write_5m_per_mtok: 3.75,
          cache_write_1h_per_mtok: 3.75,
          cache_read_per_mtok: 0.3,
        },
      ],
    })
    expect(describeTierBands(r)).toEqual(['≤ 32K: $1.2/$6', '32K–128K: $2.4/$12', '> 128K: $3/$15'])
  })

  it('returns nothing for a flat rate', () => {
    expect(describeTierBands(rate('2020-01-01T00:00:00Z'))).toEqual([])
  })
})

describe('summarizeRatePrices', () => {
  it('shows the single price for a flat rate', () => {
    expect(summarizeRatePrices(rate('2020-01-01T00:00:00Z'))).toBe('$5 / $25')
  })

  // Showing only the base band would present the cheapest price as the whole
  // price — the misreading this issue exists to end.
  it('shows the full span for a tiered rate', () => {
    expect(summarizeRatePrices(tieredRate())).toBe('$0.25 / $1.5 – $1 / $4')
  })

  it('collapses a single-band rate to one price', () => {
    const r = rate('2020-01-01T00:00:00Z', {
      tiers: [
        {
          max_input_tokens: 100000,
          input_per_mtok: 2,
          output_per_mtok: 8,
          cache_write_5m_per_mtok: 2.5,
          cache_write_1h_per_mtok: 2.5,
          cache_read_per_mtok: 0.2,
        },
      ],
    })
    expect(summarizeRatePrices(r)).toBe('$2 / $8')
  })
})
