import { useState, useEffect, useRef, useCallback } from 'react'
import { useNavigate } from 'react-router-dom'
import {
  ArrowLeft,
  ArrowRight,
  CheckCircle,
  Loader2,
  AlertCircle,
  Smartphone,
  Info,
} from 'lucide-react'
import QRCode from 'react-qr-code'
import { integrationsApi } from '@/lib/api'
import type { ServiceConfig } from '@/types'
import WhatsAppIntegrationEditor from '@/components/integrations/WhatsAppIntegrationEditor'

type Step = 1 | 2 | 3

export default function IntegrationWhatsAppPage() {
  const navigate = useNavigate()
  const [step, setStep] = useState<Step>(1)
  const [integrationId, setIntegrationId] = useState<string | null>(null)

  // Step 1 form state
  const [name, setName] = useState('')

  // Step 2 service config
  const [services, setServices] = useState<Record<string, ServiceConfig>>({
    messaging: { enabled: false, tools: [] },
  })

  // Step 3 pairing state
  const [qrCode, setQrCode] = useState<string | null>(null)
  const [pairing, setPairing] = useState(false)
  const [paired, setPaired] = useState(false)
  const [phone, setPhone] = useState<string | null>(null)
  const [pairingError, setPairingError] = useState<string | null>(null)

  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null)

  const stopPolling = useCallback(() => {
    if (pollRef.current) {
      clearInterval(pollRef.current)
      pollRef.current = null
    }
  }, [])

  // Cleanup polling on unmount
  useEffect(() => {
    return () => stopPolling()
  }, [stopPolling])

  const pollQRCode = useCallback(
    (id: string) => {
      pollRef.current = setInterval(async () => {
        try {
          const result = await integrationsApi.getWhatsAppQR(id)
          if (result.status === 'paired') {
            setPaired(true)
            setPhone(result.phone ?? null)
            setPairing(false)
            stopPolling()
          } else if (result.status === 'error') {
            setPairingError(result.error ?? 'Pairing failed')
            setPairing(false)
            stopPolling()
          } else if (result.status === 'pending' && result.qr_code) {
            setQrCode(result.qr_code)
          } else if (result.status === 'no_session') {
            // Session expired, check auth status
            const authResult = await integrationsApi.getAuthStatus(id)
            if (authResult.authenticated) {
              setPaired(true)
              setPairing(false)
              stopPolling()
            }
          }
        } catch {
          // Ignore polling errors
        }
      }, 5000)
    },
    [stopPolling],
  )

  const handleSaveAndPair = async () => {
    setSaving(true)
    setError(null)
    setPairingError(null)

    try {
      // Create the integration first (not yet authenticated).
      const created = await integrationsApi.create({
        name,
        type: 'whatsapp',
        enabled: true,
        credentials: {},
        services,
      })
      setIntegrationId(created.id)
      setStep(3)
      setPairing(true)

      // Start QR code pairing.
      const result = await integrationsApi.startWhatsAppPairing(created.id)
      setQrCode(result.qr_code)

      // Start polling for QR code updates and pairing completion.
      pollQRCode(created.id)
    } catch (err) {
      const msg = (err as Error).message
      if (step === 3 || integrationId) {
        setPairingError(msg)
        setPairing(false)
      } else {
        setError(msg)
      }
    } finally {
      setSaving(false)
    }
  }

  const handleRetryPairing = async () => {
    if (!integrationId) return
    setPairing(true)
    setPairingError(null)
    setQrCode(null)

    try {
      const result = await integrationsApi.startWhatsAppPairing(integrationId)
      setQrCode(result.qr_code)
      pollQRCode(integrationId)
    } catch (err) {
      setPairingError((err as Error).message)
      setPairing(false)
    }
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center gap-3 border-b border-zinc-100 dark:border-zinc-700/50 px-4 sm:px-6 py-4 shrink-0">
        <button
          onClick={() => navigate('/integrations')}
          className="flex items-center gap-1.5 text-sm text-zinc-500 dark:text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors cursor-pointer"
        >
          <ArrowLeft className="h-4 w-4" />
          Integrations
        </button>
        <span className="text-zinc-300 dark:text-zinc-600">/</span>
        <h1 className="text-xl font-semibold text-zinc-900 dark:text-zinc-100">
          WhatsApp Integration
        </h1>
      </div>

      {/* Scrollable content */}
      <div className="flex-1 overflow-y-auto p-4 sm:p-6">
        {/* Step indicator */}
        <div className="flex items-center gap-2 mb-6">
          {([1, 2, 3] as Step[]).map(s => (
            <div
              key={s}
              className={`h-1.5 flex-1 rounded-full transition-colors ${
                step >= s ? 'bg-zinc-900 dark:bg-zinc-100' : 'bg-zinc-200 dark:bg-zinc-700'
              }`}
            />
          ))}
        </div>

        {error && (
          <div className="rounded-md border border-red-200 dark:border-red-800/50 bg-red-50 dark:bg-red-900/20 px-4 py-3 text-sm text-red-700 dark:text-red-400 mb-4">
            {error}
          </div>
        )}

        {/* Step 1: Integration Name */}
        {step === 1 && (
          <div>
            <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-1">
              WhatsApp Linked Device
            </h2>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-4">
              Connect your WhatsApp account by scanning a QR code with your phone. This uses the
              WhatsApp Web linked device feature.
            </p>

            <div className="rounded-md border border-amber-200 dark:border-amber-800/50 bg-amber-50 dark:bg-amber-900/20 px-4 py-3 text-sm text-amber-700 dark:text-amber-400 mb-4">
              <strong>Note:</strong> This integration uses the unofficial WhatsApp Web protocol.
              There is a risk of account restrictions. Recommended for personal/development use
              only.
            </div>

            <div className="space-y-4 max-w-lg">
              <div>
                <label
                  htmlFor="whatsapp-integration-name"
                  className="block text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1"
                >
                  Integration name
                </label>
                <input
                  id="whatsapp-integration-name"
                  type="text"
                  value={name}
                  onChange={e => setName(e.target.value)}
                  placeholder="My WhatsApp"
                  className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 dark:placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400"
                />
              </div>
            </div>

            <div className="flex justify-end mt-6">
              <button
                onClick={() => setStep(2)}
                disabled={!name}
                className="flex items-center gap-1.5 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm hover:bg-zinc-700 dark:hover:bg-zinc-300 disabled:opacity-40 disabled:cursor-not-allowed transition-colors cursor-pointer"
              >
                Next
                <ArrowRight className="h-4 w-4" />
              </button>
            </div>
          </div>
        )}

        {/* Step 2: Services & Tools */}
        {step === 2 && (
          <div>
            <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-1">
              Enable services & tools
            </h2>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-4">
              Choose which WhatsApp tools agents can access.
            </p>

            <WhatsAppIntegrationEditor services={services} onServicesChange={setServices} />

            <div className="flex justify-between mt-6">
              <button
                onClick={() => setStep(1)}
                className="flex items-center gap-1.5 text-sm text-zinc-500 dark:text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors cursor-pointer"
              >
                <ArrowLeft className="h-4 w-4" />
                Back
              </button>
              <button
                onClick={handleSaveAndPair}
                disabled={saving || !Object.values(services).some(s => s.enabled)}
                className="flex items-center gap-1.5 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm hover:bg-zinc-700 dark:hover:bg-zinc-300 disabled:opacity-40 disabled:cursor-not-allowed transition-colors cursor-pointer"
              >
                {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : null}
                Save & Pair Device
              </button>
            </div>
          </div>
        )}

        {/* Step 3: QR Code Pairing */}
        {step === 3 && (
          <div className="text-center py-8">
            {pairing && !paired && (
              <>
                <div className="flex flex-col items-center gap-4 mb-6">
                  {qrCode ? (
                    <div className="p-4 bg-white rounded-lg border border-zinc-200 dark:border-zinc-700 inline-block">
                      <QRCode value={qrCode} size={256} />
                    </div>
                  ) : (
                    <Loader2 className="h-12 w-12 text-zinc-400 animate-spin" />
                  )}
                </div>
                <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-2">
                  Scan QR Code with WhatsApp
                </h2>
                <p className="text-sm text-zinc-500 dark:text-zinc-400 max-w-md mx-auto">
                  Open WhatsApp on your phone, go to <strong>Settings &gt; Linked Devices</strong>,
                  tap <strong>Link a Device</strong>, and scan the QR code displayed above. The QR
                  code refreshes automatically every ~20 seconds.
                </p>
                <div className="flex items-center justify-center gap-2 mt-4 text-xs text-zinc-400">
                  <Smartphone className="h-4 w-4" />
                  Waiting for device to be linked...
                </div>
              </>
            )}

            {!pairing && paired && (
              <>
                <CheckCircle className="h-12 w-12 text-green-500 mx-auto mb-4" />
                <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-2">
                  WhatsApp connected!
                </h2>
                <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-2">
                  Your WhatsApp device has been linked successfully.
                </p>
                {phone && (
                  <p className="text-sm text-zinc-600 dark:text-zinc-300 font-mono mb-4">
                    Phone: {phone}
                  </p>
                )}

                {/* Linked-device capability note */}
                <div className="rounded-md border border-blue-200 dark:border-blue-800/50 bg-blue-50 dark:bg-blue-900/20 px-4 py-3 text-left mb-6 max-w-md mx-auto">
                  <div className="flex gap-2.5">
                    <Info className="h-4 w-4 text-blue-600 dark:text-blue-400 shrink-0 mt-0.5" />
                    <div className="text-sm text-blue-700 dark:text-blue-300 space-y-1.5">
                      <p className="font-medium">What your agent can do right now</p>
                      <ul className="space-y-1 text-xs text-blue-600 dark:text-blue-400">
                        <li>
                          ✅ <strong>send_message</strong> — provide a phone number and the agent
                          can send messages immediately
                        </li>
                        <li>
                          ✅ <strong>send_media</strong> — send images or documents by URL
                        </li>
                        <li>
                          ⏳ <strong>list_chats / get_contacts</strong> — these read from a local
                          contact store that starts empty. WhatsApp does not sync contact history to
                          linked devices, so these populate gradually as messages flow through this
                          session.
                        </li>
                      </ul>
                      <p className="text-xs text-blue-600 dark:text-blue-400">
                        💡 Tip: tell your agent the phone number directly (e.g. "message
                        +1234567890") rather than asking it to look up contacts.
                      </p>
                    </div>
                  </div>
                </div>

                <div className="flex justify-center gap-3">
                  <button
                    onClick={() => navigate('/integrations')}
                    className="rounded-md border border-zinc-300 dark:border-zinc-600 px-4 py-2 text-sm text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors cursor-pointer"
                  >
                    Back to Integrations
                  </button>
                  {integrationId && (
                    <button
                      onClick={() => navigate(`/integrations/${integrationId}`)}
                      className="rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm hover:bg-zinc-700 dark:hover:bg-zinc-300 transition-colors cursor-pointer"
                    >
                      View Details
                    </button>
                  )}
                </div>
              </>
            )}

            {!pairing && pairingError && (
              <>
                <AlertCircle className="h-12 w-12 text-red-500 mx-auto mb-4" />
                <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-2">
                  Pairing failed
                </h2>
                <p className="text-sm text-red-600 dark:text-red-400 mb-6">{pairingError}</p>
                <div className="flex justify-center gap-3">
                  <button
                    onClick={() => navigate('/integrations')}
                    className="rounded-md border border-zinc-300 dark:border-zinc-600 px-4 py-2 text-sm text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors cursor-pointer"
                  >
                    Back to Integrations
                  </button>
                  <button
                    onClick={handleRetryPairing}
                    className="rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm hover:bg-zinc-700 dark:hover:bg-zinc-300 transition-colors cursor-pointer"
                  >
                    Retry Pairing
                  </button>
                </div>
              </>
            )}
          </div>
        )}
      </div>
    </div>
  )
}
