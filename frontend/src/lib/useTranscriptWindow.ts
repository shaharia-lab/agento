import { useCallback, useEffect, useMemo, useState } from 'react'
import type { ClaudeMessage } from '@/types'
import { hasProse, hasToolUse, searchIndex } from './transcript'
import { useDebounced } from './useDebounced'

/**
 * How many events the transcript renders before the reader asks for more.
 *
 * The endpoint ships the whole transcript — 1,219 messages and 597 KB for the
 * largest session on the reference machine, which is the median heavy session
 * rather than the tail — and rendering all of them was thousands of DOM nodes
 * before the reader had scrolled anywhere. The window extends automatically as
 * it comes into view, so this bounds the initial paint rather than the reach.
 */
export const TRANSCRIPT_WINDOW = 200

/** Matching the sessions list, and for the same reason. */
export const TRANSCRIPT_SEARCH_DEBOUNCE_MS = 250

export type TranscriptFilter = 'all' | 'messages' | 'tools'

/**
 * A request to scroll the transcript to one event.
 *
 * The nonce is what makes clicking the same timeline entry twice scroll twice:
 * the uuid alone would not change, so the effect would not re-run.
 */
export interface TranscriptJump {
  uuid: string
  nonce: number
}

export interface TranscriptWindow {
  /** The search box's value — immediate, unlike what it filters. */
  search: string
  setSearch: (value: string) => void
  filter: TranscriptFilter
  setFilter: (value: TranscriptFilter) => void
  /** Every event matching the filter and the settled search. */
  visible: ClaudeMessage[]
  /** The prefix of `visible` that is actually rendered. */
  shown: ClaudeMessage[]
  /** Extends the rendered window by one page. */
  showMore: () => void
}

/**
 * The transcript's filtering and rendering window.
 *
 * Extracted from the panel because it is the whole of what changed when the
 * transcript stopped rendering every event and re-serializing every tool input
 * per keystroke. The panel is left describing layout.
 */
export function useTranscriptWindow(
  messages: readonly ClaudeMessage[],
  jump: TranscriptJump | null,
): TranscriptWindow {
  const [search, setSearch] = useState('')
  const [filter, setFilter] = useState<TranscriptFilter>('all')

  // Only what is acted on is delayed; the input itself stays immediate.
  const debouncedSearch = useDebounced(search, TRANSCRIPT_SEARCH_DEBOUNCE_MS)

  // Built once per transcript rather than once per keystroke per message: the
  // predicate used to JSON.stringify every tool input on every letter typed.
  const haystacks = useMemo(() => searchIndex(messages), [messages])

  const visible = useMemo(
    () => filterMessages(messages, haystacks, filter, debouncedSearch),
    [messages, haystacks, filter, debouncedSearch],
  )

  // The window is scoped to the current filter, so a filter that leaves twelve
  // events starts from the top again rather than keeping a window sized for the
  // previous thousand. Derived during render rather than reset by an effect,
  // which would render the old window once before correcting it.
  const scope = `${filter} ${debouncedSearch}`
  const [window, setWindow] = useState({ scope, count: TRANSCRIPT_WINDOW })
  const base = window.scope === scope ? window.count : TRANSCRIPT_WINDOW
  const showMore = useCallback(
    () => setWindow(w => ({ scope, count: (w.scope === scope ? w.count : 0) + TRANSCRIPT_WINDOW })),
    [scope],
  )

  // A jump target outside the window has no DOM node to scroll to, so the
  // window widens to include it — in the same render, so the node exists by the
  // time the scrolling effect below runs.
  const jumpIndex = jump ? visible.findIndex(m => m.uuid === jump.uuid) : -1
  const rendered = jumpIndex >= 0 ? Math.max(base, jumpIndex + TRANSCRIPT_WINDOW) : base

  const jumpKey = jump ? `${jump.uuid} ${jump.nonce}` : ''
  useEffect(() => {
    if (!jumpKey) return
    const uuid = jumpKey.slice(0, jumpKey.lastIndexOf(' '))
    document.getElementById(`event-${uuid}`)?.scrollIntoView({ behavior: 'smooth', block: 'start' })
  }, [jumpKey])

  return {
    search,
    setSearch,
    filter,
    setFilter,
    visible,
    shown: visible.slice(0, rendered),
    showMore,
  }
}

function filterMessages(
  messages: readonly ClaudeMessage[],
  haystacks: readonly string[],
  filter: TranscriptFilter,
  search: string,
): ClaudeMessage[] {
  const q = search.trim().toLowerCase()
  return messages.filter((m, i) => {
    if (filter === 'messages' && !hasProse(m)) return false
    if (filter === 'tools' && !hasToolUse(m)) return false
    return !q || haystacks[i].includes(q)
  })
}
