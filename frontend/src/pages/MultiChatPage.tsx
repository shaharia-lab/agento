import { useState, useEffect, useCallback } from 'react'
import { useNavigate, useLocation } from 'react-router-dom'
import { chatsApi } from '@/lib/api'
import type { ChatSession } from '@/types'
import ChatSessionPage from '@/pages/ChatSessionPage'
import MultiChatTabStrip, { type TabInfo } from '@/components/MultiChatTabStrip'
import NewChatDialog from '@/components/NewChatDialog'

const STORAGE_KEY = 'agento_multi_chat_tabs'

interface StoredTabState {
  tabs: string[]
  activeTabId: string
}

function loadStoredTabs(): StoredTabState | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return null
    const parsed = JSON.parse(raw) as StoredTabState
    if (Array.isArray(parsed.tabs) && typeof parsed.activeTabId === 'string') {
      return parsed
    }
  } catch {
    // ignore corrupt data
  }
  return null
}

function saveStoredTabs(tabs: TabInfo[], activeTabId: string) {
  const state: StoredTabState = {
    tabs: tabs.map(t => t.id),
    activeTabId,
  }
  localStorage.setItem(STORAGE_KEY, JSON.stringify(state))
}

export default function MultiChatPage() {
  const navigate = useNavigate()
  const location = useLocation()
  const [tabs, setTabs] = useState<TabInfo[]>([])
  const [activeTabId, setActiveTabId] = useState<string>('')
  const [newChatDialogOpen, setNewChatDialogOpen] = useState(false)
  const [initialized, setInitialized] = useState(false)

  // On mount: restore tabs from localStorage and integrate navigation state.
  useEffect(() => {
    const navState = location.state as { fromChatId?: string } | null
    const fromChatId = navState?.fromChatId
    // Clear navigation state so refresh does not re-process it.
    if (fromChatId) {
      globalThis.history.replaceState({}, '')
    }

    const stored = loadStoredTabs()

    // Validate stored tabs by fetching them. Invalid IDs are silently dropped.
    const initTabs = async () => {
      let tabIds: string[] = stored?.tabs ?? []

      // If arriving from single-chat, ensure that chat is in the list.
      if (fromChatId && !tabIds.includes(fromChatId)) {
        tabIds = [fromChatId, ...tabIds]
      }

      // Fetch all chat details to get titles and drop invalid ones.
      const results = await Promise.allSettled(tabIds.map(id => chatsApi.get(id)))
      const validTabs: TabInfo[] = []
      for (let i = 0; i < tabIds.length; i++) {
        const r = results[i]
        if (r.status === 'fulfilled') {
          validTabs.push({ id: tabIds[i], title: r.value.session.title })
        }
      }

      if (validTabs.length === 0) {
        // No valid tabs at all — redirect to chats list.
        navigate('/chats', { replace: true })
        return
      }

      // Determine the active tab.
      let activeId = fromChatId ?? stored?.activeTabId ?? ''
      if (!validTabs.some(t => t.id === activeId)) {
        activeId = validTabs[0].id
      }

      setTabs(validTabs)
      setActiveTabId(activeId)
      saveStoredTabs(validTabs, activeId)
      setInitialized(true)

      // If arriving from single-chat [+] button, auto-open new chat dialog.
      if (fromChatId) {
        setNewChatDialogOpen(true)
      }
    }

    initTabs()
    // Only run on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // Persist tab state whenever it changes.
  useEffect(() => {
    if (initialized && tabs.length > 0) {
      saveStoredTabs(tabs, activeTabId)
    }
  }, [tabs, activeTabId, initialized])

  const handleTabSelect = useCallback((id: string) => {
    setActiveTabId(id)
  }, [])

  const handleTabClose = useCallback(
    (id: string) => {
      setTabs(prev => {
        const next = prev.filter(t => t.id !== id)
        if (next.length === 0) {
          // Last tab closed — clear storage and redirect.
          localStorage.removeItem(STORAGE_KEY)
          navigate('/chats')
          return prev
        }
        // If closing the active tab, switch to the nearest neighbor.
        if (id === activeTabId) {
          const closedIdx = prev.findIndex(t => t.id === id)
          const newActive = next[Math.min(closedIdx, next.length - 1)]
          setActiveTabId(newActive.id)
        }
        return next
      })
    },
    [activeTabId, navigate],
  )

  const handleAddTab = useCallback(() => {
    setNewChatDialogOpen(true)
  }, [])

  const handleNewChatCreated = useCallback((session: ChatSession, firstMessage: string) => {
    const newTab: TabInfo = { id: session.id, title: session.title || 'New Chat' }
    setTabs(prev => [...prev, newTab])
    setActiveTabId(session.id)
    setNewChatDialogOpen(false)

    // Send the first message by navigating with pendingMessage state.
    // Since ChatSessionPage is already mounted (or about to be), we store
    // the pending message in a way that the component can pick up.
    // We use a custom event to communicate to the specific ChatSessionPage instance.
    setTimeout(() => {
      window.dispatchEvent(
        new CustomEvent('multi-chat-pending-message', {
          detail: { chatId: session.id, message: firstMessage },
        }),
      )
    }, 100)
  }, [])

  const handleTitleChange = useCallback((chatId: string, newTitle: string) => {
    setTabs(prev => prev.map(t => (t.id === chatId ? { ...t, title: newTitle } : t)))
  }, [])

  if (!initialized) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400">Loading workspace...</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      <MultiChatTabStrip
        tabs={tabs}
        activeTabId={activeTabId}
        onTabSelect={handleTabSelect}
        onTabClose={handleTabClose}
        onAddTab={handleAddTab}
      />

      <div className="flex-1 min-h-0 overflow-hidden">
        {tabs.map(tab => (
          <div
            key={tab.id}
            className="h-full"
            style={{ display: tab.id === activeTabId ? 'block' : 'none' }}
          >
            <ChatSessionPage
              chatId={tab.id}
              onBack={() => handleTabClose(tab.id)}
              onTitleChange={title => handleTitleChange(tab.id, title)}
            />
          </div>
        ))}
      </div>

      <NewChatDialog
        open={newChatDialogOpen}
        onOpenChange={setNewChatDialogOpen}
        onCreated={handleNewChatCreated}
      />
    </div>
  )
}
