/**
 * Shared formatting utilities used across session-related pages.
 */

/** Abbreviates large token counts: 1,200,000 → "1.2M", 15,000 → "15K". */
export function formatTokens(n: number): string {
  if (!n) return '—'
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`
  return String(n)
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
