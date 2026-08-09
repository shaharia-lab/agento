/**
 * Says when the figures on screen are provisional.
 *
 * Since #208 the session list is served from cache even while a re-cost is
 * pending, which is the right trade — an 18-second wait to render a dashboard
 * is worse than a briefly stale one — but only the sessions list ever said so.
 * The analytics pages presented costs computed under a superseded pricing
 * catalog as final, which is the one thing a cost dashboard must not do.
 */
import { useEffect, useRef, useState } from 'react'
import { Loader2 } from 'lucide-react'

import { claudeSessionsApi } from '@/lib/api'

/** How often to re-check while something is pending. */
const POLL_MS = 3_000

export function ScanStatusNotice({ onSettled }: Readonly<{ onSettled?: () => void }>) {
  const [pending, setPending] = useState(false)
  const [scanning, setScanning] = useState(false)
  const wasPending = useRef(false)

  // Held in a ref, not listed as a dependency: callers pass a plain arrow
  // recreated on every render, so depending on it would tear down and restart
  // the poll each render — cancelling the scheduled timer and firing a request
  // immediately, turning a 3s interval into one request per render while a scan
  // is running. The ref keeps the latest callback without owning the schedule,
  // and is written in an effect because a render must not mutate a ref.
  const settled = useRef(onSettled)
  useEffect(() => {
    settled.current = onSettled
  }, [onSettled])

  useEffect(() => {
    let cancelled = false
    let timer: ReturnType<typeof setTimeout>

    const poll = async () => {
      try {
        const status = await claudeSessionsApi.status()
        if (cancelled) return

        const busy = status.costs_stale || status.scan_in_progress
        // Reload the page's data once the scan finishes, so the figures the
        // notice was hedging get replaced rather than left hedged.
        if (wasPending.current && !busy) settled.current?.()
        wasPending.current = busy
        setPending(busy)
        setScanning(status.scan_in_progress)
        if (busy) timer = setTimeout(poll, POLL_MS)
      } catch {
        // The notice is an affordance, not the feature: a failed status check
        // must never break the dashboard, so stop polling and say nothing.
      }
    }

    void poll()
    return () => {
      cancelled = true
      clearTimeout(timer)
    }
    // Deliberately empty: the poll owns its own schedule for the component's
    // lifetime, and the only changing input is read through a ref.
  }, [])

  if (!pending) return null

  return (
    <div className="flex items-center gap-2 rounded-md border border-amber-200 dark:border-amber-800/50 bg-amber-50 dark:bg-amber-900/20 px-3 py-2 text-xs text-amber-800 dark:text-amber-300">
      <Loader2 className="h-3.5 w-3.5 animate-spin shrink-0" />
      {scanning
        ? 'A scan is running — sessions and costs on this page may be incomplete until it finishes.'
        : 'Pricing changed since these costs were computed. They are being recalculated in the background.'}
    </div>
  )
}
