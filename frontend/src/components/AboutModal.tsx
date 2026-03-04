import { useEffect, useState } from 'react'
import { X, Github, ExternalLink, Tag, AlertCircle, BookOpen, Star } from 'lucide-react'
import { versionApi } from '@/lib/api'
import type { UpdateCheckResponse } from '@/types'

interface AboutModalProps {
  readonly onClose: () => void
}

export default function AboutModal({ onClose }: AboutModalProps) {
  const [versionInfo, setVersionInfo] = useState<UpdateCheckResponse | null>(null)

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    globalThis.addEventListener('keydown', handler)
    return () => globalThis.removeEventListener('keydown', handler)
  }, [onClose])

  useEffect(() => {
    versionApi
      .checkUpdate()
      .then(setVersionInfo)
      .catch(() => undefined)
  }, [])

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
        aria-labelledby="about-modal-title"
        className="relative m-0 p-0 w-full max-w-md rounded-xl bg-white dark:bg-zinc-900 shadow-2xl border border-zinc-200 dark:border-zinc-700 overflow-hidden"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-200 dark:border-zinc-700">
          <div className="flex items-center gap-2.5">
            <div className="h-7 w-7 rounded-md bg-zinc-900 dark:bg-zinc-100 flex items-center justify-center shrink-0">
              <span className="text-white dark:text-zinc-900 text-sm font-bold leading-none">
                A
              </span>
            </div>
            <h2
              id="about-modal-title"
              className="text-base font-semibold text-zinc-900 dark:text-zinc-100"
            >
              About Agento
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
          {/* Description */}
          <p className="text-sm text-zinc-600 dark:text-zinc-400 leading-relaxed">
            Personal AI agent platform using Claude Code. Build and run AI agents with a clean web
            UI — manage chats, configure agents, schedule tasks, and connect third-party
            integrations.
          </p>

          {/* Version */}
          <div className="flex items-start gap-3">
            <Tag className="h-4 w-4 text-zinc-400 shrink-0 mt-0.5" />
            <div className="flex flex-col gap-1 min-w-0">
              <div className="flex items-center gap-2">
                <span className="text-sm text-zinc-500 dark:text-zinc-400">Version</span>
                <span className="text-sm font-mono font-medium text-zinc-900 dark:text-zinc-100 truncate">
                  {versionInfo?.current_version ?? '—'}
                </span>
              </div>
              {versionInfo?.update_available && (
                <a
                  href={
                    versionInfo.release_url || 'https://github.com/shaharia-lab/agento/releases'
                  }
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-1 text-xs text-amber-700 dark:text-amber-400 hover:underline"
                >
                  <span className="inline-block w-1.5 h-1.5 rounded-full bg-amber-500 shrink-0" />
                  Update available — v{versionInfo.latest_version}
                </a>
              )}
            </div>
          </div>

          {/* Star CTA */}
          <a
            href="https://github.com/shaharia-lab/agento"
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-3 rounded-lg px-4 py-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 hover:bg-amber-100 dark:hover:bg-amber-900/30 transition-colors group"
          >
            <Star className="h-4 w-4 text-amber-500 shrink-0 group-hover:fill-amber-500 transition-all" />
            <span className="flex-1 text-sm text-amber-800 dark:text-amber-300 font-medium">
              Star on GitHub to support this project
            </span>
            <ExternalLink className="h-3.5 w-3.5 text-amber-400 dark:text-amber-600 shrink-0" />
          </a>

          <div className="border-t border-zinc-100 dark:border-zinc-800" />

          {/* Links */}
          <div className="space-y-2.5">
            <a
              href="https://github.com/shaharia-lab/agento"
              target="_blank"
              rel="noopener noreferrer"
              className="flex items-center gap-3 rounded-lg px-3 py-2.5 text-sm text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors group"
            >
              <Github className="h-4 w-4 text-zinc-400 group-hover:text-zinc-600 dark:group-hover:text-zinc-200 shrink-0" />
              <span className="flex-1">GitHub Repository</span>
              <ExternalLink className="h-3.5 w-3.5 text-zinc-300 dark:text-zinc-600 group-hover:text-zinc-400 dark:group-hover:text-zinc-400" />
            </a>

            <a
              href="https://github.com/shaharia-lab/agento/issues/new"
              target="_blank"
              rel="noopener noreferrer"
              className="flex items-center gap-3 rounded-lg px-3 py-2.5 text-sm text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors group"
            >
              <AlertCircle className="h-4 w-4 text-zinc-400 group-hover:text-zinc-600 dark:group-hover:text-zinc-200 shrink-0" />
              <span className="flex-1">Report an Issue</span>
              <ExternalLink className="h-3.5 w-3.5 text-zinc-300 dark:text-zinc-600 group-hover:text-zinc-400 dark:group-hover:text-zinc-400" />
            </a>

            <a
              href="https://github.com/shaharia-lab/agento/releases"
              target="_blank"
              rel="noopener noreferrer"
              className="flex items-center gap-3 rounded-lg px-3 py-2.5 text-sm text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors group"
            >
              <BookOpen className="h-4 w-4 text-zinc-400 group-hover:text-zinc-600 dark:group-hover:text-zinc-200 shrink-0" />
              <span className="flex-1">Release Notes</span>
              <ExternalLink className="h-3.5 w-3.5 text-zinc-300 dark:text-zinc-600 group-hover:text-zinc-400 dark:group-hover:text-zinc-400" />
            </a>
          </div>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-6 py-4 border-t border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/50">
          <span className="text-xs text-zinc-400 dark:text-zinc-500">
            © {new Date().getFullYear()} Shaharia Lab OÜ. MIT License.
          </span>
          <button
            onClick={onClose}
            className="px-4 py-1.5 text-sm rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 hover:bg-zinc-700 dark:hover:bg-zinc-300 transition-colors cursor-pointer"
          >
            Close
          </button>
        </div>
      </dialog>
    </div>
  )
}
