import { Switch } from '@/components/ui/switch'
import type { ServiceConfig } from '@/types'
import { GitBranch } from 'lucide-react'

interface ToolInfo {
  name: string
  description: string
}

interface ServiceInfo {
  label: string
  description: string
  tools: ToolInfo[]
}

const JIRA_SERVICES: Record<string, ServiceInfo> = {
  project_management: {
    label: 'Project Management',
    description: 'Manage projects, search/create/update issues, and transition statuses',
    tools: [
      { name: 'list_projects', description: 'List all accessible Jira projects' },
      { name: 'get_project', description: 'Get details of a specific project by key' },
      { name: 'search_issues', description: 'Search issues using JQL query language' },
      { name: 'get_issue', description: 'Get details of a specific issue by key' },
      { name: 'create_issue', description: 'Create a new issue in a project' },
      { name: 'update_issue', description: 'Update fields of an existing issue' },
      { name: 'add_comment', description: 'Add a comment to an issue' },
      { name: 'list_transitions', description: 'List available status transitions for an issue' },
      { name: 'transition_issue', description: 'Transition an issue to a new status' },
    ],
  },
}

interface JiraIntegrationEditorProps {
  readonly services: Record<string, ServiceConfig>
  readonly onServicesChange: (services: Record<string, ServiceConfig>) => void
}

export default function JiraIntegrationEditor({
  services,
  onServicesChange,
}: JiraIntegrationEditorProps) {
  const handleServiceToggle = (svcName: string) => {
    const svc = services[svcName] ?? { enabled: false, tools: [] }
    const nowEnabled = !svc.enabled
    const info = JIRA_SERVICES[svcName]
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
    <div className="grid gap-4 grid-cols-1">
      {Object.entries(JIRA_SERVICES).map(([svcName, info]) => {
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
                  <GitBranch className="h-5 w-5 text-[#0052CC]" />
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

export { JIRA_SERVICES }
