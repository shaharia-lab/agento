import { useEffect, useRef, useState } from 'react'
import { Loader2, RefreshCw, Wifi, WifiOff } from 'lucide-react'
import { integrationsApi } from '@/lib/api'

interface WhatsAppStatusPanelProps {
  readonly integrationId: string
}

type ConnectionState = 'connected' | 'reconnecting' | 'disconnected'

function connectionState(connected: boolean, loggedIn: boolean): ConnectionState {
  if (loggedIn && connected) return 'connected'
  if (loggedIn && !connected) return 'reconnecting'
  return 'disconnected'
}

const STATE_LABEL: Record<ConnectionState, string> = {
  connected: 'Connected',
  reconnecting: 'Reconnecting…',
  disconnected: 'Disconnected',
}

const STATE_DETAIL: Record<ConnectionState, string> = {
  connected: 'The agent can send messages right now.',
  reconnecting:
    'Session is valid — WhatsApp will reconnect automatically. Use Reconnect if this persists.',
  disconnected: 'No active session found. Reconnect to re-establish the WhatsApp connection.',
}

export default function WhatsAppStatusPanel({ integrationId }: WhatsAppStatusPanelProps) {
  const [state, setState] = useState<ConnectionState>('disconnected')
  const [reconnecting, setReconnecting] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null)

  useEffect(() => {
    const poll = () => {
      integrationsApi
        .getWhatsAppStatus(integrationId)
        .then(s => setState(connectionState(s.connected, s.logged_in)))
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
      // Give the server a moment to restart the MCP server, then refresh status.
      setTimeout(() => {
        integrationsApi
          .getWhatsAppStatus(integrationId)
          .then(s => setState(connectionState(s.connected, s.logged_in)))
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

  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800 p-4">
      <h2 className="text-xs font-semibold uppercase tracking-widest text-zinc-400 mb-3">
        Connection Status
      </h2>
      {error && <p className="text-xs text-red-600 dark:text-red-400 mb-2">{error}</p>}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          {state === 'connected' ? (
            <Wifi className="h-5 w-5 text-green-500" />
          ) : state === 'reconnecting' ? (
            <Wifi className="h-5 w-5 text-amber-500" />
          ) : (
            <WifiOff className="h-5 w-5 text-zinc-400" />
          )}
          <div>
            <p
              className={`text-sm font-medium ${
                state === 'connected'
                  ? 'text-green-700 dark:text-green-400'
                  : state === 'reconnecting'
                    ? 'text-amber-700 dark:text-amber-400'
                    : 'text-zinc-900 dark:text-zinc-100'
              }`}
            >
              {STATE_LABEL[state]}
            </p>
            <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">{STATE_DETAIL[state]}</p>
          </div>
        </div>
        <button
          onClick={handleReconnect}
          disabled={reconnecting || state === 'connected'}
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
