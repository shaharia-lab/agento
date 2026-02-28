import { GoogleCalendarIcon, GmailIcon, GoogleDriveIcon } from '@/components/GoogleIcons'
import { Switch } from '@/components/ui/switch'
import type { ServiceConfig } from '@/types'
import type { ReactNode } from 'react'

interface ToolInfo {
  name: string
  description: string
}

interface ServiceInfo {
  label: string
  description: string
  icon: ReactNode
  tools: ToolInfo[]
}

const GOOGLE_SERVICES: Record<string, ServiceInfo> = {
  calendar: {
    label: 'Google Calendar',
    description: 'Manage events, check availability, and schedule meetings',
    icon: <GoogleCalendarIcon size={20} />,
    tools: [
      {
        name: 'create_event',
        description: 'Create new calendar events with attendees and details',
      },
      { name: 'view_events', description: 'List and search upcoming or past events' },
    ],
  },
  gmail: {
    label: 'Gmail',
    description: 'Read, send, and search email messages',
    icon: <GmailIcon size={20} />,
    tools: [
      { name: 'send_email', description: 'Compose and send email messages' },
      { name: 'read_email', description: 'Read the content of specific emails' },
      { name: 'search_email', description: 'Search across your mailbox by query' },
    ],
  },
  drive: {
    label: 'Google Drive',
    description: 'Browse, create, and download files from Drive',
    icon: <GoogleDriveIcon size={20} />,
    tools: [
      { name: 'list_files', description: 'List and search for files and folders' },
      { name: 'create_file', description: 'Create new documents and files' },
      { name: 'download_file', description: 'Download file contents from Drive' },
    ],
  },
}

interface GoogleIntegrationEditorProps {
  services: Record<string, ServiceConfig>
  onServicesChange: (services: Record<string, ServiceConfig>) => void
}

export default function GoogleIntegrationEditor({
  services,
  onServicesChange,
}: GoogleIntegrationEditorProps) {
  const handleServiceToggle = (svcName: string) => {
    const svc = services[svcName] ?? { enabled: false, tools: [] }
    const nowEnabled = !svc.enabled
    const info = GOOGLE_SERVICES[svcName]
    onServicesChange({
      ...services,
      [svcName]: {
        enabled: nowEnabled,
        tools: nowEnabled ? info.tools.map(t => t.name) : [],
      },
    })
  }

  const handleToolToggle = (svcName: string, tool: string) => {
    const svc = services[svcName] ?? { enabled: true, tools: [] }
    const tools = svc.tools.includes(tool)
      ? svc.tools.filter(t => t !== tool)
      : [...svc.tools, tool]
    onServicesChange({ ...services, [svcName]: { ...svc, tools } })
  }

  return (
    <div className="grid gap-4 grid-cols-1 lg:grid-cols-2">
      {Object.entries(GOOGLE_SERVICES).map(([svcName, info]) => {
        const svc = services[svcName] ?? { enabled: false, tools: [] }

        return (
          <div
            key={svcName}
            className={`rounded-lg border p-4 transition-colors ${
              svc.enabled
                ? 'border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800'
                : 'border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/50'
            }`}
          >
            {/* Service header */}
            <div className="flex items-start justify-between gap-3 mb-1">
              <div className="flex items-center gap-2.5">
                <div className="flex h-8 w-8 items-center justify-center rounded-md border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 shrink-0">
                  {info.icon}
                </div>
                <div>
                  <p className="text-sm font-medium text-zinc-900 dark:text-zinc-100">
                    {info.label}
                  </p>
                </div>
              </div>
              <Switch checked={svc.enabled} onCheckedChange={() => handleServiceToggle(svcName)} />
            </div>
            <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-3 ml-[42px]">
              {info.description}
            </p>

            {/* Tools list */}
            <div className="space-y-2 ml-[42px]">
              {info.tools.map(tool => {
                const isChecked = svc.tools.includes(tool.name)
                return (
                  <label
                    key={tool.name}
                    className={`flex items-start gap-2.5 rounded-md p-2 -mx-2 transition-colors cursor-pointer ${
                      svc.enabled
                        ? 'hover:bg-zinc-50 dark:hover:bg-zinc-700/50'
                        : 'opacity-50 cursor-not-allowed'
                    }`}
                  >
                    <input
                      type="checkbox"
                      checked={isChecked}
                      onChange={() => handleToolToggle(svcName, tool.name)}
                      disabled={!svc.enabled}
                      aria-label={tool.name}
                      className="h-3.5 w-3.5 rounded border-zinc-300 dark:border-zinc-600 mt-0.5 shrink-0 disabled:opacity-40"
                    />
                    <div className="min-w-0">
                      <p
                        className={`text-sm font-mono leading-tight ${
                          svc.enabled
                            ? 'text-zinc-900 dark:text-zinc-100'
                            : 'text-zinc-400 dark:text-zinc-500'
                        }`}
                      >
                        {tool.name}
                      </p>
                      <p
                        className={`text-xs mt-0.5 ${
                          svc.enabled
                            ? 'text-zinc-500 dark:text-zinc-400'
                            : 'text-zinc-400 dark:text-zinc-500'
                        }`}
                      >
                        {tool.description}
                      </p>
                    </div>
                  </label>
                )
              })}
            </div>
          </div>
        )
      })}
    </div>
  )
}

export { GOOGLE_SERVICES }
