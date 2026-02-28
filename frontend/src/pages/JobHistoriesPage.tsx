import { useState, useEffect, useCallback } from 'react'
import { useNavigate } from 'react-router-dom'
import { jobHistoryApi } from '@/lib/api'
import type { JobHistoryEntry } from '@/types'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { ClipboardList, ExternalLink } from 'lucide-react'

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`
  const sec = ms / 1000
  if (sec < 60) return `${sec.toFixed(1)}s`
  const min = sec / 60
  return `${min.toFixed(1)}m`
}

function JobStatusBadge({ status }: Readonly<{ status: string }>) {
  const colors: Record<string, string> = {
    success: 'bg-green-50 text-green-700 dark:bg-green-900/30 dark:text-green-400',
    failed: 'bg-red-50 text-red-700 dark:bg-red-900/30 dark:text-red-400',
    running: 'bg-blue-50 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400',
  }
  return (
    <span
      className={`inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${colors[status] ?? 'bg-zinc-100 text-zinc-500'}`}
    >
      {status}
    </span>
  )
}

function DetailRow({ label, children }: Readonly<{ label: string; children: React.ReactNode }>) {
  return (
    <div className="flex flex-col gap-0.5">
      <dt className="text-xs font-medium text-zinc-500 dark:text-zinc-400">{label}</dt>
      <dd className="text-sm text-zinc-900 dark:text-zinc-100">{children}</dd>
    </div>
  )
}

const PAGE_SIZE = 50

export default function JobHistoriesPage() {
  const navigate = useNavigate()
  const [entries, setEntries] = useState<JobHistoryEntry[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [offset, setOffset] = useState(0)
  const [hasMore, setHasMore] = useState(true)
  const [selected, setSelected] = useState<JobHistoryEntry | null>(null)

  const load = useCallback(async (currentOffset: number) => {
    try {
      setLoading(true)
      const data = await jobHistoryApi.list(PAGE_SIZE, currentOffset)
      if (currentOffset === 0) {
        setEntries(data)
      } else {
        setEntries(prev => [...prev, ...data])
      }
      setHasMore(data.length === PAGE_SIZE)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load job history')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    load(0)
  }, [load])

  const loadMore = () => {
    const newOffset = offset + PAGE_SIZE
    setOffset(newOffset)
    load(newOffset)
  }

  if (loading && entries.length === 0) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400 dark:text-zinc-500">Loading job history…</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between border-b border-zinc-100 dark:border-zinc-800 px-4 sm:px-6 py-4 shrink-0">
        <div>
          <h1 className="text-base font-semibold text-zinc-900 dark:text-zinc-100">Job History</h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {entries.length} entr{entries.length === 1 ? 'y' : 'ies'}
          </p>
        </div>
      </div>

      {error && (
        <div className="mx-6 mt-3 rounded-md border border-red-200 bg-red-50 dark:border-red-800 dark:bg-red-900/20 px-4 py-2.5 text-sm text-red-700 dark:text-red-400">
          {error}
        </div>
      )}

      <div className="flex-1 overflow-y-auto">
        {entries.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-20 text-center">
            <div className="flex h-12 w-12 items-center justify-center rounded-full bg-zinc-100 dark:bg-zinc-800 mb-4">
              <ClipboardList className="h-5 w-5 text-zinc-400 dark:text-zinc-500" />
            </div>
            <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100 mb-1">
              No job history
            </h2>
            <p className="text-xs text-zinc-500 dark:text-zinc-400 max-w-xs">
              Job executions will appear here once scheduled tasks start running.
            </p>
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-zinc-100 dark:border-zinc-800 text-xs text-zinc-500 dark:text-zinc-400">
                  <th className="text-left px-4 py-2.5 font-medium">Task</th>
                  <th className="text-left px-4 py-2.5 font-medium">Status</th>
                  <th className="text-left px-4 py-2.5 font-medium">Started</th>
                  <th className="text-left px-4 py-2.5 font-medium">Duration</th>
                  <th className="text-left px-4 py-2.5 font-medium">Tokens</th>
                </tr>
              </thead>
              <tbody>
                {entries.map(entry => (
                  <tr
                    key={entry.id}
                    onClick={() => setSelected(entry)}
                    className="border-b border-zinc-50 dark:border-zinc-800/50 hover:bg-zinc-50 dark:hover:bg-zinc-800/50 transition-colors cursor-pointer"
                  >
                    <td className="px-4 py-2.5">
                      <div className="font-medium text-zinc-900 dark:text-zinc-100 text-xs">
                        {entry.task_name}
                      </div>
                      {entry.agent_slug && (
                        <div className="text-xs text-zinc-400 dark:text-zinc-500 font-mono">
                          {entry.agent_slug}
                        </div>
                      )}
                    </td>
                    <td className="px-4 py-2.5">
                      <JobStatusBadge status={entry.status} />
                    </td>
                    <td className="px-4 py-2.5 text-xs text-zinc-500 dark:text-zinc-400 whitespace-nowrap">
                      {new Date(entry.started_at).toLocaleString()}
                    </td>
                    <td className="px-4 py-2.5 text-xs text-zinc-500 dark:text-zinc-400 font-mono">
                      {entry.duration_ms > 0 ? formatDuration(entry.duration_ms) : '-'}
                    </td>
                    <td className="px-4 py-2.5 text-xs text-zinc-500 dark:text-zinc-400 font-mono">
                      {entry.total_input_tokens + entry.total_output_tokens > 0
                        ? `${(entry.total_input_tokens + entry.total_output_tokens).toLocaleString()}`
                        : '-'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>

            {hasMore && (
              <div className="flex justify-center py-4">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={loadMore}
                  disabled={loading}
                  className="text-xs"
                >
                  {loading ? 'Loading…' : 'Load more'}
                </Button>
              </div>
            )}
          </div>
        )}
      </div>

      {/* Detail dialog */}
      <Dialog
        open={selected !== null}
        onOpenChange={open => {
          if (!open) setSelected(null)
        }}
      >
        <DialogContent className="max-w-lg">
          {selected && (
            <>
              <DialogHeader>
                <DialogTitle className="flex items-center gap-2">
                  {selected.task_name}
                  <JobStatusBadge status={selected.status} />
                </DialogTitle>
              </DialogHeader>

              <dl className="grid grid-cols-2 gap-x-4 gap-y-3 mt-2">
                <DetailRow label="Started">
                  {new Date(selected.started_at).toLocaleString()}
                </DetailRow>
                <DetailRow label="Duration">
                  {selected.duration_ms > 0 ? formatDuration(selected.duration_ms) : '-'}
                </DetailRow>
                {selected.agent_slug && (
                  <DetailRow label="Agent">
                    <span className="font-mono">{selected.agent_slug}</span>
                  </DetailRow>
                )}
                {selected.model && (
                  <DetailRow label="Model">
                    <span className="font-mono">{selected.model}</span>
                  </DetailRow>
                )}
                <DetailRow label="Input tokens">
                  {selected.total_input_tokens.toLocaleString()}
                </DetailRow>
                <DetailRow label="Output tokens">
                  {selected.total_output_tokens.toLocaleString()}
                </DetailRow>
                {(selected.total_cache_read_tokens > 0 ||
                  selected.total_cache_creation_tokens > 0) && (
                  <>
                    <DetailRow label="Cache read">
                      {selected.total_cache_read_tokens.toLocaleString()}
                    </DetailRow>
                    <DetailRow label="Cache creation">
                      {selected.total_cache_creation_tokens.toLocaleString()}
                    </DetailRow>
                  </>
                )}
              </dl>

              {selected.prompt_preview && (
                <div className="mt-3">
                  <p className="text-xs font-medium text-zinc-500 dark:text-zinc-400 mb-1">
                    Prompt
                  </p>
                  <p className="text-sm text-zinc-700 dark:text-zinc-300 bg-zinc-50 dark:bg-zinc-800 rounded-md px-3 py-2 whitespace-pre-wrap break-words">
                    {selected.prompt_preview}
                  </p>
                </div>
              )}

              {selected.error_message && (
                <div className="mt-3">
                  <p className="text-xs font-medium text-red-500 dark:text-red-400 mb-1">Error</p>
                  <p className="text-sm text-red-700 dark:text-red-300 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-md px-3 py-2 whitespace-pre-wrap break-words">
                    {selected.error_message}
                  </p>
                </div>
              )}

              {selected.chat_session_id && (
                <div className="mt-4 pt-3 border-t border-zinc-100 dark:border-zinc-800">
                  <Button
                    size="sm"
                    onClick={() => {
                      setSelected(null)
                      navigate(`/chats/${selected.chat_session_id}`)
                    }}
                    className="gap-1.5 bg-zinc-900 hover:bg-zinc-800 dark:bg-zinc-100 dark:hover:bg-zinc-200 dark:text-zinc-900 text-white text-xs h-8"
                  >
                    <ExternalLink className="h-3.5 w-3.5" />
                    View Chat Session
                  </Button>
                </div>
              )}
            </>
          )}
        </DialogContent>
      </Dialog>
    </div>
  )
}
