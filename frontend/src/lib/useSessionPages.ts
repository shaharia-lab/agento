import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { claudeSessionsApi } from '@/lib/api'
import type { ClaudeSessionFacets, ClaudeSessionSummary } from '@/types'
import { toQueryParams, type SessionFilters, type SessionSort } from './sessionQuery'

/** Sessions fetched per page. */
export const PAGE_SIZE = 50

export interface SessionPages {
  /** The pages loaded so far, in server order. Never the whole corpus. */
  sessions: ClaudeSessionSummary[]
  /** Totals and filter options across the whole filtered set. */
  facets: ClaudeSessionFacets | null
  loading: boolean
  loadingMore: boolean
  hasMore: boolean
  error: string | null
  /** Discards the loaded pages and fetches the first one again. */
  reload: () => Promise<void>
  /** Appends the next page. A no-op while one is in flight or none is left. */
  loadMore: () => Promise<void>
  /** Applies a local edit — a favourite toggle — to the loaded rows. */
  patchSession: (sessionId: string, patch: Partial<ClaudeSessionSummary>) => void
  setError: (message: string | null) => void
}

/**
 * The sessions list's data: one page at a time, refetched when the filter or
 * the sort changes.
 *
 * Extracted from the page component because it is the whole of what changed
 * when the list stopped being a client-side view over the corpus. Keeping it
 * here means the component describes layout and the hook describes paging,
 * rather than one function doing both.
 */
export function useSessionPages(filters: SessionFilters, sort: SessionSort): SessionPages {
  const [sessions, setSessions] = useState<ClaudeSessionSummary[]>([])
  const [facets, setFacets] = useState<ClaudeSessionFacets | null>(null)
  const [cursor, setCursor] = useState('')
  const [hasMore, setHasMore] = useState(false)
  const [loading, setLoading] = useState(true)
  const [loadingMore, setLoadingMore] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // Serialized once and compared as a string: URLSearchParams is a fresh object
  // on every render, so an effect keyed on the object would refetch forever.
  const queryKey = useMemo(() => toQueryParams(filters).toString(), [filters])

  /**
   * Guards against an out-of-order response: a slow request for a filter the
   * user has already moved on from must not overwrite what they are looking at
   * now. Incrementing invalidates every request in flight.
   */
  const generation = useRef(0)

  const reload = useCallback(async () => {
    const mine = ++generation.current
    setLoading(true)
    try {
      const params = new URLSearchParams(queryKey)
      const [page, f] = await Promise.all([
        claudeSessionsApi.list({ filters: params, sort, limit: PAGE_SIZE }),
        claudeSessionsApi.facets(params),
      ])
      if (mine !== generation.current) return
      setSessions(page.items)
      setCursor(page.next_cursor)
      setHasMore(page.has_more)
      setFacets(f)
      setError(null)
    } catch (err) {
      if (mine !== generation.current) return
      setError(err instanceof Error ? err.message : 'Failed to load sessions')
    } finally {
      if (mine === generation.current) setLoading(false)
    }
  }, [queryKey, sort])

  useEffect(() => {
    reload()
  }, [reload])

  const loadMore = useCallback(async () => {
    if (!cursor || loadingMore) return
    const mine = generation.current
    setLoadingMore(true)
    try {
      const page = await claudeSessionsApi.list({
        filters: new URLSearchParams(queryKey),
        sort,
        limit: PAGE_SIZE,
        cursor,
      })
      if (mine !== generation.current) return
      // Concatenated rather than merged: the server returns a strict keyset
      // continuation, so a row cannot arrive twice and none can be skipped.
      setSessions(prev => [...prev, ...page.items])
      setCursor(page.next_cursor)
      setHasMore(page.has_more)
    } catch (err) {
      if (mine === generation.current) {
        setError(err instanceof Error ? err.message : 'Failed to load more sessions')
      }
    } finally {
      if (mine === generation.current) setLoadingMore(false)
    }
  }, [cursor, loadingMore, queryKey, sort])

  const patchSession = useCallback((sessionId: string, patch: Partial<ClaudeSessionSummary>) => {
    setSessions(prev => prev.map(s => (s.session_id === sessionId ? { ...s, ...patch } : s)))
  }, [])

  return {
    sessions,
    facets,
    loading,
    loadingMore,
    hasMore,
    error,
    reload,
    loadMore,
    patchSession,
    setError,
  }
}

/**
 * The count the advanced panel's unapplied draft would leave, so "Apply" is
 * never a leap in the dark.
 *
 * One cheap aggregate rather than a second full predicate pass over the corpus,
 * and only while the panel is open. A failed request leaves the previous count
 * rather than showing zero, which would read as "this filter matches nothing".
 */
export function useDraftMatchCount(draftKey: string | null): number {
  const [count, setCount] = useState(0)

  useEffect(() => {
    if (draftKey === null) return
    let cancelled = false
    claudeSessionsApi
      .facets(new URLSearchParams(draftKey))
      .then(f => {
        if (!cancelled) setCount(f.total)
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [draftKey])

  return count
}
