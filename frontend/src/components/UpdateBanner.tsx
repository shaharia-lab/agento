import { useState, useEffect } from 'react'
import { X, ArrowUpCircle, Terminal, Download, RotateCcw } from 'lucide-react'
import { versionApi } from '@/lib/api'
import type { UpdateCheckResponse } from '@/types'

// TODO: remove ?force devtools button before merging to PR.
const DEV_FORCE = new URLSearchParams(window.location.search).get('devtools') === '1'

interface HowToUpdateModalProps {
  latestVersion: string
  releaseUrl: string
  onClose: () => void
}

function HowToUpdateModal({ latestVersion, releaseUrl, onClose }: HowToUpdateModalProps) {
  // Close on Escape
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [onClose])

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/50 backdrop-blur-sm"
      onClick={e => {
        if (e.target === e.currentTarget) onClose()
      }}
    >
      <div className="relative w-full max-w-lg rounded-xl bg-white dark:bg-zinc-900 shadow-2xl border border-zinc-200 dark:border-zinc-700 overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-200 dark:border-zinc-700">
          <div className="flex items-center gap-2.5">
            <ArrowUpCircle className="h-5 w-5 text-amber-500" />
            <h2 className="text-base font-semibold text-zinc-900 dark:text-zinc-100">
              How to update Agento
            </h2>
          </div>
          <button
            onClick={onClose}
            aria-label="Close"
            className="h-7 w-7 flex items-center justify-center rounded-md text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
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
            className="px-4 py-1.5 text-sm rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 hover:bg-zinc-700 dark:hover:bg-zinc-300 transition-colors"
          >
            Got it
          </button>
        </div>
      </div>
    </div>
  )
}

export default function UpdateBanner() {
  const [info, setInfo] = useState<UpdateCheckResponse | null>(null)
  const [dismissed, setDismissed] = useState(false)
  const [modalOpen, setModalOpen] = useState(false)
  // TODO: remove before merging — lets QA force-show the banner via ?devtools=1.
  const [forceShow, setForceShow] = useState(false)

  useEffect(() => {
    versionApi
      .checkUpdate()
      .then(setInfo)
      .catch(() => undefined)
  }, [])

  const show = !dismissed && (forceShow || info?.update_available === true)
  const latestVersion = forceShow ? 'v99.99.99' : (info?.latest_version ?? '')
  const releaseUrl = forceShow
    ? 'https://github.com/shaharia-lab/agento/releases'
    : (info?.release_url ?? '')

  if (!show && !DEV_FORCE) return null

  return (
    <>
      {/* TODO: remove this dev button before merging */}
      {DEV_FORCE && !forceShow && !info?.update_available && (
        <div className="flex justify-center bg-zinc-100 dark:bg-zinc-800 px-4 py-1">
          <button
            onClick={() => setForceShow(true)}
            className="text-xs text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-300 underline"
          >
            [dev] show update banner
          </button>
        </div>
      )}

      {show && (
        <div className="flex items-center gap-3 px-4 py-2 bg-amber-50 dark:bg-amber-950/40 border-b border-amber-200 dark:border-amber-800 text-sm text-amber-900 dark:text-amber-200 shrink-0">
          <ArrowUpCircle className="h-4 w-4 text-amber-500 dark:text-amber-400 shrink-0" />
          <span className="flex-1">
            A new version of Agento is available
            {latestVersion && (
              <>
                {' '}
                <strong>{latestVersion}</strong>
              </>
            )}
            .{' '}
            <button
              onClick={() => setModalOpen(true)}
              className="underline hover:no-underline cursor-pointer"
            >
              How to update
            </button>
            {releaseUrl && (
              <>
                {' · '}
                <a
                  href={releaseUrl}
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
            onClick={() => setDismissed(true)}
            aria-label="Dismiss update notification"
            className="shrink-0 h-6 w-6 flex items-center justify-center rounded hover:bg-amber-100 dark:hover:bg-amber-900/50 transition-colors"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      {modalOpen && (
        <HowToUpdateModal
          latestVersion={latestVersion}
          releaseUrl={releaseUrl}
          onClose={() => setModalOpen(false)}
        />
      )}
    </>
  )
}
