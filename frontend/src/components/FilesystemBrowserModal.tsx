import { useState, useEffect, useCallback } from 'react'
import { Folder, FolderOpen, ChevronRight, Loader2 } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { filesystemApi } from '@/lib/api'
import type { FSListResponse } from '@/types'

interface FilesystemBrowserModalProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  initialPath?: string
  onSelect: (path: string) => void
}

export default function FilesystemBrowserModal({
  open,
  onOpenChange,
  initialPath,
  onSelect,
}: FilesystemBrowserModalProps) {
  const [current, setCurrent] = useState<FSListResponse | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const navigate = useCallback(async (path?: string) => {
    setLoading(true)
    setError(null)
    try {
      const data = await filesystemApi.list(path)
      setCurrent(data)
    } catch {
      setError('Could not read directory')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    if (open) {
      void navigate(initialPath)
    }
  }, [open, initialPath, navigate])

  const handleSelect = () => {
    if (current) {
      onSelect(current.path)
      onOpenChange(false)
    }
  }

  const breadcrumbs = current ? buildBreadcrumbs(current.path) : []

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Browse Directories</DialogTitle>
        </DialogHeader>

        {/* Breadcrumb */}
        {current && (
          <div className="flex items-center gap-1 flex-wrap text-xs text-zinc-500 min-h-[20px]">
            {breadcrumbs.map((crumb, i) => (
              <span key={crumb.path} className="flex items-center gap-1">
                {i > 0 && <ChevronRight className="h-3 w-3 shrink-0" />}
                <button
                  onClick={() => void navigate(crumb.path)}
                  className="hover:text-zinc-900 transition-colors truncate max-w-[120px]"
                  title={crumb.label}
                >
                  {crumb.label}
                </button>
              </span>
            ))}
          </div>
        )}

        {/* Directory listing */}
        <div className="border border-zinc-200 rounded-md overflow-hidden">
          {/* Go up */}
          {current && current.path !== current.parent && (
            <button
              onClick={() => void navigate(current.parent)}
              className="flex items-center gap-2 w-full px-3 py-2 text-sm text-zinc-500 hover:bg-zinc-50 border-b border-zinc-100 transition-colors"
            >
              <Folder className="h-4 w-4 shrink-0 text-zinc-400" />
              <span className="font-mono">..</span>
            </button>
          )}

          <div className="max-h-60 overflow-y-auto">
            {loading && (
              <div className="flex items-center justify-center py-8">
                <Loader2 className="h-5 w-5 animate-spin text-zinc-400" />
              </div>
            )}
            {!loading && error && <div className="px-4 py-3 text-sm text-red-600">{error}</div>}
            {!loading && !error && current?.entries.length === 0 && (
              <div className="px-4 py-3 text-sm text-zinc-400">No subdirectories</div>
            )}
            {!loading &&
              !error &&
              current?.entries.map(entry => (
                <button
                  key={entry.path}
                  onClick={() => void navigate(entry.path)}
                  className="flex items-center gap-2 w-full px-3 py-2 text-sm text-zinc-700 hover:bg-zinc-50 border-b border-zinc-50 last:border-0 transition-colors text-left"
                >
                  <FolderOpen className="h-4 w-4 shrink-0 text-zinc-400" />
                  <span className="truncate">{entry.name}</span>
                </button>
              ))}
          </div>
        </div>

        {/* Current path display */}
        {current && (
          <div className="text-xs text-zinc-500 font-mono truncate px-1">{current.path}</div>
        )}

        <DialogFooter className="gap-2">
          <Button variant="outline" size="sm" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            size="sm"
            className="bg-zinc-900 hover:bg-zinc-800 text-white"
            onClick={handleSelect}
            disabled={!current}
          >
            Select this directory
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function buildBreadcrumbs(path: string): { label: string; path: string }[] {
  const parts = path.split('/').filter(Boolean)
  const crumbs: { label: string; path: string }[] = [{ label: '/', path: '/' }]
  let accumulated = ''
  for (const part of parts) {
    accumulated += '/' + part
    crumbs.push({ label: part, path: accumulated })
  }
  return crumbs
}
