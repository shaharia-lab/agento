import { describe, it, expect } from 'vitest'
import { formatCost, formatTokens } from './format'

describe('formatCost', () => {
  it('prints an exact zero as $0.00', () => {
    // A session of purely non-billable models (synthetic, embeddings) really is free.
    expect(formatCost(0)).toBe('$0.00')
  })

  it('never rounds a real cost down to $0.00', () => {
    // The failure this guards: a 3K-token session is cheap, not free.
    expect(formatCost(0.015)).toBe('$0.0150')
    expect(formatCost(0.004)).toBe('$0.0040')
    expect(formatCost(0.0001)).toBe('$0.0001')
  })

  it('floors amounts too small to show at four decimals', () => {
    expect(formatCost(0.00001)).toBe('< $0.0001')
  })

  it('keeps four decimals below a dollar and two above', () => {
    expect(formatCost(0.1234)).toBe('$0.1234')
    expect(formatCost(1)).toBe('$1.00')
    expect(formatCost(1.239)).toBe('$1.24')
  })

  it('groups thousands', () => {
    expect(formatCost(1234.5)).toBe('$1,234.50')
  })

  it('treats a negative as zero rather than printing -$', () => {
    expect(formatCost(-1)).toBe('$0.00')
  })
})

describe('formatTokens', () => {
  it('renders an em dash for zero', () => {
    expect(formatTokens(0)).toBe('—')
  })

  it('abbreviates thousands and millions', () => {
    expect(formatTokens(999)).toBe('999')
    expect(formatTokens(15_000)).toBe('15K')
    expect(formatTokens(1_200_000)).toBe('1.2M')
  })
})

describe('formatTokens at billions', () => {
  it('uses B rather than a four-digit M', () => {
    // A month of cache reads reaches this magnitude; "21274.5M" is both
    // unreadable and wide enough to clip the chart axis it labels.
    expect(formatTokens(21_274_518_062)).toBe('21.3B')
    expect(formatTokens(1_000_000_000)).toBe('1.0B')
    expect(formatTokens(999_999_999)).toBe('1000.0M')
  })
})
