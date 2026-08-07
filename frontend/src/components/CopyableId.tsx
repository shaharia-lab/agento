import { type MouseEvent, useCallback, useEffect, useRef, useState } from 'react'
import { Copy, Check } from 'lucide-react'
import { cn } from '@/lib/utils'

/**
 * Renders an identifier (e.g. a session UUID) in full and copies it to the
 * clipboard when clicked, without triggering any surrounding navigation.
 *
 * Reuses the clipboard + transient-feedback pattern from JsonPreview in
 * ClaudeSettingsTab.tsx. Safe to nest inside a clickable row: the click handler
 * calls stopPropagation and no-ops when navigator.clipboard is unavailable
 * (e.g. a non-secure context) rather than throwing.
 *
 * The copy icon reveals on hover of either the component itself (group/copyable)
 * or a surrounding `group` container, so it works standalone in a page header as
 * well as inside a hoverable list row.
 */
export function CopyableId({
  value,
  label = 'Copy ID',
  className,
}: Readonly<{ value: string; label?: string; className?: string }>) {
  const [copied, setCopied] = useState(false)
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    return () => {
      if (timeoutRef.current) clearTimeout(timeoutRef.current)
    }
  }, [])

  const handleCopy = useCallback(
    async (e: MouseEvent<HTMLButtonElement>) => {
      e.stopPropagation()
      if (!navigator.clipboard) return
      try {
        await navigator.clipboard.writeText(value)
      } catch {
        return
      }
      setCopied(true)
      if (timeoutRef.current) clearTimeout(timeoutRef.current)
      timeoutRef.current = setTimeout(() => setCopied(false), 2000)
    },
    [value],
  )

  return (
    <button
      type="button"
      onClick={handleCopy}
      title={copied ? 'Copied!' : label}
      aria-label={copied ? 'Copied!' : label}
      className={cn(
        'group/copyable inline-flex items-start gap-1 text-left font-mono text-xs break-all text-zinc-400 dark:text-zinc-500 hover:text-zinc-600 dark:hover:text-zinc-300 transition-colors',
        className,
      )}
    >
      <span className="break-all">{value}</span>
      {copied ? (
        <Check className="h-3 w-3 shrink-0 mt-0.5 text-emerald-500" />
      ) : (
        <Copy className="h-3 w-3 shrink-0 mt-0.5 opacity-0 group-hover:opacity-100 group-hover/copyable:opacity-100 transition-opacity" />
      )}
    </button>
  )
}
