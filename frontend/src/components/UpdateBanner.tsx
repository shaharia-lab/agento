import { useState, useEffect } from 'react'
import { X, ArrowUpCircle, Terminal, Download, RotateCcw } from 'lucide-react'
import { versionApi } from '@/lib/api'
import type { UpdateCheckResponse } from '@/types'

const DISMISS_STORAGE_KEY = 'agento-update-dismissed-version'

interface HowToUpdateModalProps {
  readonly latestVersion: string
  readonly releaseUrl: string
  readonly onClose: () => void
}

function HowToUpdateModal({ latestVersion, releaseUrl, onClose }: HowToUpdateModalProps) {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    globalThis.addEventListener('keydown', handler)
    return () => globalThis.removeEventListener('keydown', handler)
  }, [onClose])

  return (
    <div
      role="none"
      className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/50 backdrop-blur-sm"
      onClick={e => {
        if (e.target === e.currentTarget) onClose()
      }}
    >
      <dialog
        open
        aria-labelledby="update-modal-title"
        className="relative m-0 p-0 w-full max-w-lg rounded-xl bg-white dark:bg-zinc-900 shadow-2xl border border-zinc-200 dark:border-zinc-700 overflow-hidden"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-200 dark:border-zinc-700">
          <div className="flex items-center gap-2.5">
            <ArrowUpCircle className="h-5 w-5 text-amber-500" />
            <h2
              id="update-modal-title"
              className="text-base font-semibold text-zinc-900 dark:text-zinc-100"
            >
              How to update Agento
            </h2>
          </div>
          <button
            onClick={onClose}
            aria-label="Close"
            className="h-7 w-7 flex items-center justify-center rounded-md text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors cursor-pointer"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {/* Body */}
        <div className="px-6 py-5 space-y-5">
          {latestVersion && (
            <p className="text-sm text-zinc-600 dark:text-zinc-400">
              Version <strong className="text-zinc-900 dark:text-zinc-100">{latestVersion}</strong>{' '}
              is available. Follow one of the methods below to upgrade.
            </p>
          )}

          {/* Method 1: built-in update command */}
          <div className="space-y-2">
            <div className="flex items-center gap-2 text-sm font-medium text-zinc-800 dark:text-zinc-200">
              <Terminal className="h-4 w-4 text-zinc-500" />
              <span>Option 1 — Automatic update (recommended)</span>
            </div>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 pl-6">
              Run the built-in update command. It downloads the latest binary and replaces the
              current one in place.
            </p>
            <div className="pl-6">
              <code className="block w-full rounded-lg bg-zinc-900 dark:bg-zinc-950 text-green-400 text-sm font-mono px-4 py-3 select-all">
                agento update
              </code>
              <p className="mt-1.5 text-xs text-zinc-400 dark:text-zinc-500">
                Add <span className="font-mono">-y</span> to skip the confirmation prompt:{' '}
                <span className="font-mono">agento update -y</span>
              </p>
            </div>
          </div>

          <div className="border-t border-zinc-100 dark:border-zinc-800" />

          {/* Method 2: manual download */}
          <div className="space-y-2">
            <div className="flex items-center gap-2 text-sm font-medium text-zinc-800 dark:text-zinc-200">
              <Download className="h-4 w-4 text-zinc-500" />
              <span>Option 2 — Manual download</span>
            </div>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 pl-6">
              Download the pre-built binary for your platform from the GitHub releases page and
              replace the existing <span className="font-mono text-xs">agento</span> binary in your{' '}
              <span className="font-mono text-xs">PATH</span>.
            </p>
            {releaseUrl && (
              <div className="pl-6">
                <a
                  href={releaseUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-1.5 text-sm text-amber-600 dark:text-amber-400 hover:underline"
                >
                  <Download className="h-3.5 w-3.5" />
                  Open release on GitHub
                </a>
              </div>
            )}
          </div>

          <div className="border-t border-zinc-100 dark:border-zinc-800" />

          {/* Restart note */}
          <div className="flex items-start gap-2 text-sm text-zinc-500 dark:text-zinc-400">
            <RotateCcw className="h-4 w-4 mt-0.5 shrink-0 text-zinc-400" />
            <span>
              After updating,{' '}
              <strong className="text-zinc-700 dark:text-zinc-300">restart Agento</strong> to start
              using the new version.
            </span>
          </div>
        </div>

        {/* Footer */}
        <div className="flex justify-end px-6 py-4 border-t border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/50">
          <button
            onClick={onClose}
            className="px-4 py-1.5 text-sm rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 hover:bg-zinc-700 dark:hover:bg-zinc-300 transition-colors cursor-pointer"
          >
            Got it
          </button>
        </div>
      </dialog>
    </div>
  )
}

export default function UpdateBanner() {
  const [info, setInfo] = useState<UpdateCheckResponse | null>(null)
  const [modalOpen, setModalOpen] = useState(false)
  // Track which version the user dismissed (keyed by version string so a new
  // release automatically re-shows the banner). Initialised from localStorage
  // on mount so it survives navigation and page refreshes.
  const [dismissedVersion, setDismissedVersion] = useState<string>(() => {
    try {
      return localStorage.getItem(DISMISS_STORAGE_KEY) ?? ''
    } catch {
      return ''
    }
  })

  useEffect(() => {
    const check = () =>
      versionApi
        .checkUpdate()
        .then(setInfo)
        .catch(() => undefined)

    check()
    const timer = setInterval(check, 60 * 60 * 1000) // re-check every hour
    return () => clearInterval(timer)
  }, [])

  const handleDismiss = () => {
    const version = info?.latest_version ?? ''
    try {
      localStorage.setItem(DISMISS_STORAGE_KEY, version)
    } catch {
      // ignore localStorage errors (e.g. private browsing restrictions)
    }
    setDismissedVersion(version)
  }

  const show = info?.update_available === true && dismissedVersion !== info.latest_version

  if (!show) return null

  return (
    <>
      <div className="flex items-center gap-3 px-4 py-2 bg-amber-50 dark:bg-amber-950/40 border-b border-amber-200 dark:border-amber-800 text-sm text-amber-900 dark:text-amber-200 shrink-0">
        <ArrowUpCircle className="h-4 w-4 text-amber-500 dark:text-amber-400 shrink-0" />
        <span className="flex-1">
          A new version of Agento is available
          {info.latest_version && (
            <>
              {' '}
              <strong>{info.latest_version}</strong>
            </>
          )}
          .{' '}
          <button
            onClick={() => setModalOpen(true)}
            className="underline hover:no-underline cursor-pointer"
          >
            How to update
          </button>
          {info.release_url && (
            <>
              {' · '}
              <a
                href={info.release_url}
                target="_blank"
                rel="noopener noreferrer"
                className="underline hover:no-underline"
              >
                Release notes
              </a>
            </>
          )}
        </span>
        <button
          onClick={handleDismiss}
          aria-label="Dismiss update notification"
          className="shrink-0 h-6 w-6 flex items-center justify-center rounded hover:bg-amber-100 dark:hover:bg-amber-900/50 transition-colors cursor-pointer"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>

      {modalOpen && (
        <HowToUpdateModal
          latestVersion={info.latest_version}
          releaseUrl={info.release_url}
          onClose={() => setModalOpen(false)}
        />
      )}
    </>
  )
}
