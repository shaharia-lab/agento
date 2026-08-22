import { useState } from 'react'
import { X, Sunset, ExternalLink } from 'lucide-react'
import {
  SUNSET_CUTOFF,
  DESKTOP_RELEASES_URL,
  SHARED_DB_PATH,
  SUNSET_DISMISS_STORAGE_KEY,
} from '@/lib/sunset'

/**
 * Announces that the Go/web build of Agento is being retired.
 *
 * Deliberately unlike UpdateBanner in three ways, each of them a constraint on
 * this release rather than a style choice:
 *
 *  - It is **static**. No fetch, no polling interval — the facts are compiled
 *    in, so the notice cannot fail to appear because a request failed.
 *  - Dismissal is **permanent**. UpdateBanner keys its dismissal on the version
 *    so a new release re-shows it; there is no such key here and nothing
 *    re-arms the banner.
 *  - It is **never modal and never blocking**. The web build keeps working
 *    indefinitely past the cutoff; staying on it is the user's call.
 */
export default function SunsetBanner() {
  const [dismissed, setDismissed] = useState<boolean>(() => {
    try {
      return localStorage.getItem(SUNSET_DISMISS_STORAGE_KEY) === '1'
    } catch {
      // Private browsing and similar restricted contexts: show the banner
      // rather than swallow it.
      return false
    }
  })

  const handleDismiss = () => {
    try {
      localStorage.setItem(SUNSET_DISMISS_STORAGE_KEY, '1')
    } catch {
      // Ignore storage errors — the banner still closes for this session.
    }
    setDismissed(true)
  }

  if (dismissed) return null

  return (
    <div className="flex items-start gap-3 px-4 py-2.5 bg-sky-50 dark:bg-sky-950/40 border-b border-sky-200 dark:border-sky-800 text-sm text-sky-900 dark:text-sky-200 shrink-0">
      <Sunset className="h-4 w-4 mt-0.5 text-sky-500 dark:text-sky-400 shrink-0" />
      <span className="flex-1">
        <strong>Agento (web) is being retired.</strong> This is the final release of the Go/web
        build — updates stop after <strong>{SUNSET_CUTOFF}</strong>. The app keeps working after
        that date; only updating stops. Agento Desktop reads the same{' '}
        <span className="font-mono text-xs">{SHARED_DB_PATH}</span>, so there is no export and no
        migration.{' '}
        <a
          href={DESKTOP_RELEASES_URL}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-1 underline hover:no-underline font-medium"
        >
          Get Agento Desktop
          <ExternalLink className="h-3 w-3" />
        </a>
      </span>
      <button
        onClick={handleDismiss}
        aria-label="Dismiss the Agento web retirement notice"
        className="shrink-0 h-6 w-6 flex items-center justify-center rounded hover:bg-sky-100 dark:hover:bg-sky-900/50 transition-colors cursor-pointer"
      >
        <X className="h-3.5 w-3.5" />
      </button>
    </div>
  )
}
