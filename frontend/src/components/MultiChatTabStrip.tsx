import { useState } from 'react'
import { Plus, X, Copy, Check } from 'lucide-react'

export interface TabInfo {
  id: string
  title: string
}

interface MultiChatTabStripProps {
  readonly tabs: TabInfo[]
  readonly activeTabId: string
  readonly onTabSelect: (id: string) => void
  readonly onTabClose: (id: string) => void
  readonly onAddTab: () => void
}

export default function MultiChatTabStrip({
  tabs,
  activeTabId,
  onTabSelect,
  onTabClose,
  onAddTab,
}: MultiChatTabStripProps) {
  const [copiedId, setCopiedId] = useState<string | null>(null)

  const copyLink = (e: React.MouseEvent, tabId: string) => {
    e.stopPropagation()
    const url = `${globalThis.location.origin}/chats/${tabId}`
    navigator.clipboard
      .writeText(url)
      .then(() => {
        setCopiedId(tabId)
        setTimeout(() => setCopiedId(null), 2000)
      })
      .catch(() => {
        // Clipboard API may fail in insecure contexts; silently ignore.
      })
  }

  return (
    <div
      role="tablist"
      className="flex items-center gap-1 border-b border-zinc-100 dark:border-zinc-700/50 px-2 py-1.5 shrink-0 overflow-x-auto"
    >
      {tabs.map(tab => {
        const isActive = tab.id === activeTabId
        return (
          <div
            key={tab.id}
            role="tab"
            tabIndex={0}
            aria-selected={isActive}
            onClick={() => onTabSelect(tab.id)}
            onKeyDown={e => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault()
                onTabSelect(tab.id)
              }
            }}
            className={`group flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs cursor-pointer transition-colors max-w-[200px] min-w-[120px] ${
              isActive
                ? 'bg-zinc-100 dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 font-medium'
                : 'text-zinc-500 dark:text-zinc-400 hover:bg-zinc-50 dark:hover:bg-zinc-800/50 hover:text-zinc-700 dark:hover:text-zinc-300'
            }`}
          >
            <span className="truncate flex-1">{tab.title || 'New Chat'}</span>
            <button
              onClick={e => copyLink(e, tab.id)}
              className="h-4 w-4 flex items-center justify-center rounded opacity-0 group-hover:opacity-100 text-zinc-400 dark:text-zinc-500 hover:text-zinc-600 dark:hover:text-zinc-300 transition-all shrink-0 cursor-pointer"
              title="Copy chat link"
            >
              {copiedId === tab.id ? (
                <Check className="h-3 w-3 text-green-500" />
              ) : (
                <Copy className="h-3 w-3" />
              )}
            </button>
            <button
              onClick={e => {
                e.stopPropagation()
                onTabClose(tab.id)
              }}
              className="h-4 w-4 flex items-center justify-center rounded opacity-0 group-hover:opacity-100 text-zinc-400 dark:text-zinc-500 hover:text-red-500 transition-all shrink-0 cursor-pointer"
              title="Close tab"
            >
              <X className="h-3 w-3" />
            </button>
          </div>
        )
      })}
      <button
        onClick={onAddTab}
        className="flex items-center gap-1.5 px-2.5 h-7 rounded-md text-xs text-zinc-500 dark:text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors shrink-0 font-medium cursor-pointer"
        title="Add new chat tab"
      >
        <Plus className="h-3.5 w-3.5" />
        New Chat Tab
      </button>
    </div>
  )
}
