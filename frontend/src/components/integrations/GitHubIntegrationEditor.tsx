import { Switch } from '@/components/ui/switch'
import type { ServiceConfig } from '@/types'
import { Github } from 'lucide-react'

interface ToolInfo {
  name: string
  description: string
}

interface ServiceInfo {
  label: string
  description: string
  icon: string
  tools: ToolInfo[]
}

const GITHUB_SERVICES: Record<string, ServiceInfo> = {
  repos: {
    label: 'Repositories',
    description: 'List repos, get repo details, and search code',
    icon: 'repos',
    tools: [
      { name: 'list_repos', description: 'List repositories for the authenticated user' },
      { name: 'get_repo', description: 'Get details of a specific repository' },
      { name: 'search_code', description: 'Search for code across GitHub repositories' },
    ],
  },
  issues: {
    label: 'Issues',
    description: 'List, create, and update issues',
    icon: 'issues',
    tools: [
      { name: 'list_issues', description: 'List issues for a repository' },
      { name: 'get_issue', description: 'Get details of a specific issue' },
      { name: 'create_issue', description: 'Create a new issue in a repository' },
      { name: 'update_issue', description: 'Update an existing issue' },
    ],
  },
  pull_requests: {
    label: 'Pull Requests',
    description: 'List, create, and review pull requests',
    icon: 'pulls',
    tools: [
      { name: 'list_pulls', description: 'List pull requests for a repository' },
      { name: 'get_pull', description: 'Get details of a specific pull request' },
      { name: 'create_pull', description: 'Create a new pull request' },
      { name: 'get_pull_diff', description: 'Get the diff of a pull request' },
      { name: 'list_pull_comments', description: 'List review comments on a pull request' },
    ],
  },
  actions: {
    label: 'Actions',
    description: 'Manage workflows, runs, and logs',
    icon: 'actions',
    tools: [
      { name: 'list_workflows', description: 'List all workflows in a repository' },
      { name: 'list_workflow_runs', description: 'List workflow runs for a repository' },
      { name: 'trigger_workflow', description: 'Trigger a workflow dispatch event' },
      { name: 'get_workflow_run', description: 'Get details of a specific workflow run' },
      { name: 'get_run_logs', description: 'Get the logs URL for a workflow run' },
    ],
  },
  releases: {
    label: 'Releases',
    description: 'Manage releases and tags',
    icon: 'releases',
    tools: [
      { name: 'list_releases', description: 'List releases for a repository' },
      { name: 'create_release', description: 'Create a new release' },
      { name: 'list_tags', description: 'List tags for a repository' },
    ],
  },
}

interface GitHubIntegrationEditorProps {
  readonly services: Record<string, ServiceConfig>
  readonly onServicesChange: (services: Record<string, ServiceConfig>) => void
}

export default function GitHubIntegrationEditor({
  services,
  onServicesChange,
}: GitHubIntegrationEditorProps) {
  const handleServiceToggle = (svcName: string) => {
    const svc = services[svcName] ?? { enabled: false, tools: [] }
    const nowEnabled = !svc.enabled
    const info = GITHUB_SERVICES[svcName]
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
      {Object.entries(GITHUB_SERVICES).map(([svcName, info]) => {
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
                  <Github className="h-5 w-5 text-zinc-800 dark:text-zinc-200" />
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

export { GITHUB_SERVICES }
