import { useEffect, useRef, useState } from 'react'
import { Loader2, RefreshCw, Wifi, WifiOff } from 'lucide-react'
import { integrationsApi } from '@/lib/api'

interface WhatsAppStatusPanelProps {
  readonly integrationId: string
}

export default function WhatsAppStatusPanel({ integrationId }: WhatsAppStatusPanelProps) {
  const [connected, setConnected] = useState(false)
  const [loggedIn, setLoggedIn] = useState(false)
  const [reconnecting, setReconnecting] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null)

  useEffect(() => {
    const poll = () => {
      integrationsApi
        .getWhatsAppStatus(integrationId)
        .then(s => {
          setConnected(s.connected)
          setLoggedIn(s.logged_in)
        })
        .catch(() => {
          /* ignore poll errors */
        })
    }

    poll()
    pollRef.current = setInterval(poll, 10000)
    return () => {
      if (pollRef.current) clearInterval(pollRef.current)
    }
  }, [integrationId])

  const handleReconnect = async () => {
    setReconnecting(true)
    setError(null)
    try {
      await integrationsApi.whatsAppReconnect(integrationId)
      // Give the server a moment to restart, then refresh status.
      setTimeout(() => {
        integrationsApi
          .getWhatsAppStatus(integrationId)
          .then(s => {
            setConnected(s.connected)
            setLoggedIn(s.logged_in)
          })
          .catch(() => {
            /* ignore */
          })
        setReconnecting(false)
      }, 3000)
    } catch (err) {
      setError((err as Error).message)
      setReconnecting(false)
    }
  }

  const statusLabel =
    connected && loggedIn
      ? 'Connected & logged in'
      : connected
        ? 'Connected but not logged in'
        : 'Disconnected'

  const statusDetail =
    connected && loggedIn
      ? 'The agent can send messages right now.'
      : 'Use Reconnect to re-establish the WhatsApp WebSocket session.'

  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800 p-4">
      <h2 className="text-xs font-semibold uppercase tracking-widest text-zinc-400 mb-3">
        Connection Status
      </h2>
      {error && <p className="text-xs text-red-600 dark:text-red-400 mb-2">{error}</p>}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          {connected && loggedIn ? (
            <Wifi className="h-5 w-5 text-green-500" />
          ) : (
            <WifiOff className="h-5 w-5 text-zinc-400" />
          )}
          <div>
            <p className="text-sm font-medium text-zinc-900 dark:text-zinc-100">{statusLabel}</p>
            <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">{statusDetail}</p>
          </div>
        </div>
        <button
          onClick={handleReconnect}
          disabled={reconnecting}
          className="flex items-center gap-1.5 rounded-md border border-zinc-200 dark:border-zinc-600 px-3 py-1.5 text-xs text-zinc-600 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-800 disabled:opacity-40 transition-colors cursor-pointer"
        >
          {reconnecting ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <RefreshCw className="h-3.5 w-3.5" />
          )}
          {reconnecting ? 'Reconnecting…' : 'Reconnect'}
        </button>
      </div>
    </div>
  )
}
