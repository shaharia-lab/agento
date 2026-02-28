import { useState, useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { agentsApi, tasksApi } from '@/lib/api'
import type { Agent, ScheduledTask, ScheduleType, ScheduleConfig } from '@/types'
import { MODELS } from '@/types'
import { Button } from '@/components/ui/button'

/** Convert an ISO/UTC string to a `YYYY-MM-DDTHH:MM` value in the user's local timezone. */
function toLocalDatetimeString(iso: string): string {
  const d = new Date(iso)
  if (isNaN(d.getTime())) return ''
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`
}

interface TaskFormProps {
  readonly initialData?: ScheduledTask
  readonly isEdit?: boolean
}

export default function TaskForm({ initialData, isEdit }: TaskFormProps) {
  const navigate = useNavigate()
  const [agents, setAgents] = useState<Agent[]>([])
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [name, setName] = useState(initialData?.name ?? '')
  const [description, setDescription] = useState(initialData?.description ?? '')
  const [prompt, setPrompt] = useState(initialData?.prompt ?? '')
  const [agentSlug, setAgentSlug] = useState(initialData?.agent_slug ?? '')
  const [model, setModel] = useState(initialData?.model ?? '')
  const [workingDirectory, setWorkingDirectory] = useState(initialData?.working_directory ?? '')
  const [timeoutMinutes, setTimeoutMinutes] = useState(initialData?.timeout_minutes ?? 30)
  const [scheduleType, setScheduleType] = useState<ScheduleType>(
    initialData?.schedule_type ?? 'one_off',
  )
  const [scheduleConfig, setScheduleConfig] = useState<ScheduleConfig>(
    initialData?.schedule_config ?? {},
  )
  const [stopAfterCount, setStopAfterCount] = useState(initialData?.stop_after_count ?? 0)
  const [stopAfterTime, setStopAfterTime] = useState(initialData?.stop_after_time ?? '')

  useEffect(() => {
    agentsApi
      .list()
      .then(setAgents)
      .catch(() => {})
  }, [])

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setSaving(true)
    setError(null)

    const data: Partial<ScheduledTask> = {
      name,
      description,
      prompt,
      agent_slug: agentSlug,
      model,
      working_directory: workingDirectory,
      timeout_minutes: timeoutMinutes,
      schedule_type: scheduleType,
      schedule_config: scheduleConfig,
      stop_after_count: stopAfterCount,
      stop_after_time: stopAfterTime || undefined,
    }

    try {
      if (isEdit && initialData) {
        await tasksApi.update(initialData.id, data)
      } else {
        await tasksApi.create(data)
      }
      navigate('/tasks')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save task')
    } finally {
      setSaving(false)
    }
  }

  const selectClass =
    'w-full rounded-md border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 focus:outline-none focus:ring-2 focus:ring-zinc-400 dark:focus:ring-zinc-500'
  const inputClass = selectClass
  const labelClass = 'block text-xs font-medium text-zinc-700 dark:text-zinc-300 mb-1'

  return (
    <form onSubmit={handleSubmit} className="flex flex-col h-full">
      <div className="flex items-center justify-between border-b border-zinc-100 dark:border-zinc-800 px-4 sm:px-6 py-4 shrink-0">
        <div>
          <h1 className="text-base font-semibold text-zinc-900 dark:text-zinc-100">
            {isEdit ? 'Edit Task' : 'New Scheduled Task'}
          </h1>
        </div>
        <div className="flex gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => navigate('/tasks')}
            className="text-xs h-8"
          >
            Cancel
          </Button>
          <Button
            type="submit"
            size="sm"
            disabled={saving}
            className="gap-1.5 bg-zinc-900 hover:bg-zinc-800 dark:bg-zinc-100 dark:hover:bg-zinc-200 dark:text-zinc-900 text-white text-xs h-8"
          >
            {saving ? 'Saving…' : isEdit ? 'Update Task' : 'Create Task'}
          </Button>
        </div>
      </div>

      {error && (
        <div className="mx-6 mt-3 rounded-md border border-red-200 bg-red-50 dark:border-red-800 dark:bg-red-900/20 px-4 py-2.5 text-sm text-red-700 dark:text-red-400">
          {error}
        </div>
      )}

      <div className="flex-1 overflow-y-auto p-4 sm:p-6">
        <div className="grid grid-cols-1 lg:grid-cols-[3fr_2fr] gap-6 max-w-6xl">
          {/* Left: Prompt */}
          <div className="space-y-4">
            <div>
              <label className={labelClass}>Prompt</label>
              <textarea
                value={prompt}
                onChange={e => setPrompt(e.target.value)}
                rows={16}
                placeholder="Enter the prompt to send to the agent. You can use {{current_date}} and {{current_time}} variables."
                className={`${inputClass} resize-y font-mono text-xs leading-relaxed`}
                required
              />
            </div>
          </div>

          {/* Right: Config */}
          <div className="space-y-5">
            {/* Basic */}
            <section className="space-y-3">
              <h3 className="text-xs font-semibold text-zinc-500 dark:text-zinc-400 uppercase tracking-wider">
                Basic
              </h3>
              <div>
                <label className={labelClass}>Name</label>
                <input
                  type="text"
                  value={name}
                  onChange={e => setName(e.target.value)}
                  placeholder="My scheduled task"
                  className={inputClass}
                  required
                />
              </div>
              <div>
                <label className={labelClass}>Description</label>
                <input
                  type="text"
                  value={description}
                  onChange={e => setDescription(e.target.value)}
                  placeholder="Optional description"
                  className={inputClass}
                />
              </div>
              <div>
                <label className={labelClass}>Agent (optional)</label>
                <select
                  value={agentSlug}
                  onChange={e => setAgentSlug(e.target.value)}
                  className={selectClass}
                >
                  <option value="">No agent (direct chat)</option>
                  {agents.map(a => (
                    <option key={a.slug} value={a.slug}>
                      {a.name}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label className={labelClass}>Model (optional)</label>
                <select
                  value={model}
                  onChange={e => setModel(e.target.value)}
                  className={selectClass}
                >
                  <option value="">Default</option>
                  {MODELS.map(m => (
                    <option key={m.value} value={m.value}>
                      {m.label}
                    </option>
                  ))}
                </select>
              </div>
            </section>

            {/* Schedule */}
            <section className="space-y-3">
              <h3 className="text-xs font-semibold text-zinc-500 dark:text-zinc-400 uppercase tracking-wider">
                Schedule
              </h3>
              <div>
                <label className={labelClass}>Type</label>
                <select
                  value={scheduleType}
                  onChange={e => {
                    setScheduleType(e.target.value as ScheduleType)
                    setScheduleConfig({})
                  }}
                  className={selectClass}
                >
                  <option value="one_off">One-off</option>
                  <option value="interval">Interval</option>
                  <option value="cron">Cron</option>
                </select>
              </div>

              {scheduleType === 'one_off' && (
                <div>
                  <label className={labelClass}>Run at</label>
                  <input
                    type="datetime-local"
                    value={
                      scheduleConfig.run_at ? toLocalDatetimeString(scheduleConfig.run_at) : ''
                    }
                    onChange={e =>
                      setScheduleConfig({
                        ...scheduleConfig,
                        run_at: e.target.value ? new Date(e.target.value).toISOString() : '',
                      })
                    }
                    className={inputClass}
                    required
                  />
                </div>
              )}

              {scheduleType === 'interval' && (
                <div className="space-y-2">
                  <div className="grid grid-cols-3 gap-2">
                    <div>
                      <label className={labelClass}>Minutes</label>
                      <input
                        type="number"
                        min={0}
                        value={scheduleConfig.every_minutes ?? ''}
                        onChange={e =>
                          setScheduleConfig({
                            ...scheduleConfig,
                            every_minutes: Number(e.target.value) || undefined,
                          })
                        }
                        placeholder="0"
                        className={inputClass}
                      />
                    </div>
                    <div>
                      <label className={labelClass}>Hours</label>
                      <input
                        type="number"
                        min={0}
                        value={scheduleConfig.every_hours ?? ''}
                        onChange={e =>
                          setScheduleConfig({
                            ...scheduleConfig,
                            every_hours: Number(e.target.value) || undefined,
                          })
                        }
                        placeholder="0"
                        className={inputClass}
                      />
                    </div>
                    <div>
                      <label className={labelClass}>Days</label>
                      <input
                        type="number"
                        min={0}
                        value={scheduleConfig.every_days ?? ''}
                        onChange={e =>
                          setScheduleConfig({
                            ...scheduleConfig,
                            every_days: Number(e.target.value) || undefined,
                          })
                        }
                        placeholder="0"
                        className={inputClass}
                      />
                    </div>
                  </div>
                  {(scheduleConfig.every_days ?? 0) > 0 && (
                    <div>
                      <label className={labelClass}>At time (HH:MM)</label>
                      <input
                        type="time"
                        value={scheduleConfig.at_time ?? ''}
                        onChange={e =>
                          setScheduleConfig({ ...scheduleConfig, at_time: e.target.value })
                        }
                        className={inputClass}
                      />
                    </div>
                  )}
                </div>
              )}

              {scheduleType === 'cron' && (
                <div>
                  <label className={labelClass}>Cron expression</label>
                  <input
                    type="text"
                    value={scheduleConfig.expression ?? ''}
                    onChange={e =>
                      setScheduleConfig({ ...scheduleConfig, expression: e.target.value })
                    }
                    placeholder="0 */2 * * *"
                    className={`${inputClass} font-mono`}
                    required
                  />
                  <p className="text-xs text-zinc-400 dark:text-zinc-500 mt-1">
                    Standard 5-field cron expression
                  </p>
                </div>
              )}
            </section>

            {/* Stop Conditions */}
            <section className="space-y-3">
              <h3 className="text-xs font-semibold text-zinc-500 dark:text-zinc-400 uppercase tracking-wider">
                Stop Conditions
              </h3>
              <div>
                <label className={labelClass}>Stop after N runs (0 = unlimited)</label>
                <input
                  type="number"
                  min={0}
                  value={stopAfterCount}
                  onChange={e => setStopAfterCount(Number(e.target.value))}
                  className={inputClass}
                />
              </div>
              <div>
                <label className={labelClass}>Stop after date (optional)</label>
                <input
                  type="datetime-local"
                  value={stopAfterTime ? toLocalDatetimeString(stopAfterTime) : ''}
                  onChange={e =>
                    setStopAfterTime(e.target.value ? new Date(e.target.value).toISOString() : '')
                  }
                  className={inputClass}
                />
              </div>
            </section>

            {/* Advanced */}
            <section className="space-y-3">
              <h3 className="text-xs font-semibold text-zinc-500 dark:text-zinc-400 uppercase tracking-wider">
                Advanced
              </h3>
              <div>
                <label className={labelClass}>Working directory</label>
                <input
                  type="text"
                  value={workingDirectory}
                  onChange={e => setWorkingDirectory(e.target.value)}
                  placeholder="/path/to/project"
                  className={inputClass}
                />
              </div>
              <div>
                <label className={labelClass}>Timeout (minutes, 1-240)</label>
                <input
                  type="number"
                  min={1}
                  max={240}
                  value={timeoutMinutes}
                  onChange={e => setTimeoutMinutes(Number(e.target.value))}
                  className={inputClass}
                />
              </div>
            </section>
          </div>
        </div>
      </div>
    </form>
  )
}
