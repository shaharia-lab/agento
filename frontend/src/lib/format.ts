/**
 * Shared formatting utilities used across session-related pages.
 */

/**
 * Abbreviates large token counts: 21,274,518,062 → "21.3B", 1,200,000 → "1.2M",
 * 15,000 → "15K".
 *
 * Billions are a real magnitude here — a month of cache reads reaches them —
 * and rendering one as "21274.5M" is unreadable.
 */
export function formatTokens(n: number): string {
  if (!n) return '—'
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`
  return String(n)
}

const usdFmt2 = new Intl.NumberFormat('en-US', {
  style: 'currency',
  currency: 'USD',
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
})
const usdFmt4 = new Intl.NumberFormat('en-US', {
  style: 'currency',
  currency: 'USD',
  minimumFractionDigits: 4,
  maximumFractionDigits: 4,
})

/**
 * Formats a USD cost: 1234.5 → "$1,234.56", 0.0042 → "$0.0042", 0.00001 → "< $0.0001".
 *
 * Precision widens below a dollar because per-session costs live there — rounding
 * a real session to "$0.00" would read as free. Only an exactly-zero cost, which a
 * session of purely non-billable models genuinely has, prints as $0.00.
 */
export function formatCost(n: number): string {
  if (n <= 0) return usdFmt2.format(0)
  if (n < 0.0001) return `< ${usdFmt4.format(0.0001)}`
  if (n < 1) return usdFmt4.format(n)
  return usdFmt2.format(n)
}

/** Formats a millisecond duration into a human-readable string: "1h 23m", "45s", "320ms". */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`
  const s = Math.floor(ms / 1000)
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  const rem = s % 60
  if (m < 60) return rem > 0 ? `${m}m ${rem}s` : `${m}m`
  const h = Math.floor(m / 60)
  const remM = m % 60
  return remM > 0 ? `${h}h ${remM}m` : `${h}h`
}

/**
 * Shortens a filesystem path by replacing the home directory prefix with "~".
 * Handles Linux (/home/user/), macOS (/Users/user/), and Windows (C:\Users\user\).
 */
export function shortPath(path: string): string {
  return path
    .replace(/^\/home\/[^/]+\//, '~/')
    .replace(/^\/Users\/[^/]+\//, '~/')
    .replace(/^[A-Za-z]:\\Users\\[^\\]+\\/, '~\\')
}
