import { useState, useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { tasksApi } from '@/lib/api'
import type { ScheduledTask } from '@/types'
import { Button } from '@/components/ui/button'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
import { Plus, CalendarClock, Pencil, Trash2, Pause, Play } from 'lucide-react'

function formatSchedule(task: ScheduledTask): string {
  const cfg = task.schedule_config
  switch (task.schedule_type) {
    case 'one_off':
      return cfg.run_at ? `Once at ${new Date(cfg.run_at).toLocaleString()}` : 'One-off'
    case 'interval':
      if (cfg.every_minutes) return `Every ${cfg.every_minutes} min`
      if (cfg.every_hours) return `Every ${cfg.every_hours} hr`
      if (cfg.every_days)
        return `Every ${cfg.every_days} day${cfg.every_days > 1 ? 's' : ''}${cfg.at_time ? ` at ${cfg.at_time}` : ''}`
      return 'Interval'
    case 'cron':
      return cfg.expression || 'Cron'
    default:
      return task.schedule_type
  }
}

function StatusBadge({ status }: Readonly<{ status: string }>) {
  const isActive = status === 'active'
  return (
    <span
      className={`inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${
        isActive
          ? 'bg-green-50 text-green-700 dark:bg-green-900/30 dark:text-green-400'
          : 'bg-zinc-100 text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400'
      }`}
    >
      {isActive ? 'Active' : 'Paused'}
    </span>
  )
}

function RunStatusBadge({ status }: Readonly<{ status: string }>) {
  if (!status) return null
  const colors: Record<string, string> = {
    success: 'bg-green-50 text-green-700 dark:bg-green-900/30 dark:text-green-400',
    failed: 'bg-red-50 text-red-700 dark:bg-red-900/30 dark:text-red-400',
    running: 'bg-blue-50 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400',
  }
  return (
    <span
      className={`inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${colors[status] ?? 'bg-zinc-100 text-zinc-500'}`}
    >
      {status}
    </span>
  )
}

export default function TasksPage() {
  const navigate = useNavigate()
  const [tasks, setTasks] = useState<ScheduledTask[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const loadTasks = async () => {
    try {
      const data = await tasksApi.list()
      setTasks(data)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load tasks')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    loadTasks()
  }, [])

  const handleDelete = async (id: string) => {
    try {
      await tasksApi.delete(id)
      setTasks(prev => prev.filter(t => t.id !== id))
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete task')
    }
  }

  const handleTogglePause = async (task: ScheduledTask) => {
    try {
      const updated =
        task.status === 'active' ? await tasksApi.pause(task.id) : await tasksApi.resume(task.id)
      setTasks(prev => prev.map(t => (t.id === task.id ? updated : t)))
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to update task')
    }
  }

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400 dark:text-zinc-500">Loading tasks…</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between border-b border-zinc-100 dark:border-zinc-800 px-4 sm:px-6 py-4 shrink-0">
        <div>
          <h1 className="text-base font-semibold text-zinc-900 dark:text-zinc-100">
            Scheduled Tasks
          </h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {tasks.length} task{tasks.length === 1 ? '' : 's'}
          </p>
        </div>
        <Button
          onClick={() => navigate('/tasks/new')}
          size="sm"
          className="gap-1.5 bg-zinc-900 hover:bg-zinc-800 dark:bg-zinc-100 dark:hover:bg-zinc-200 dark:text-zinc-900 text-white text-xs h-8"
        >
          <Plus className="h-3.5 w-3.5" />
          New Task
        </Button>
      </div>

      {error && (
        <div className="mx-6 mt-3 rounded-md border border-red-200 bg-red-50 dark:border-red-800 dark:bg-red-900/20 px-4 py-2.5 text-sm text-red-700 dark:text-red-400">
          {error}
        </div>
      )}

      <div className="flex-1 overflow-y-auto p-4 sm:p-6">
        {tasks.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-20 text-center">
            <div className="flex h-12 w-12 items-center justify-center rounded-full bg-zinc-100 dark:bg-zinc-800 mb-4">
              <CalendarClock className="h-5 w-5 text-zinc-400 dark:text-zinc-500" />
            </div>
            <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100 mb-1">
              No scheduled tasks
            </h2>
            <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-4 max-w-xs">
              Schedule agent tasks to run automatically — one-time or recurring.
            </p>
            <Button
              onClick={() => navigate('/tasks/new')}
              size="sm"
              className="gap-1.5 bg-zinc-900 hover:bg-zinc-800 dark:bg-zinc-100 dark:hover:bg-zinc-200 dark:text-zinc-900 text-white text-xs h-8"
            >
              <Plus className="h-3.5 w-3.5" />
              Create your first task
            </Button>
          </div>
        ) : (
          <div className="grid gap-3 grid-cols-1 sm:grid-cols-2 xl:grid-cols-3">
            {tasks.map(task => (
              <TaskCard
                key={task.id}
                task={task}
                onEdit={() => navigate(`/tasks/${task.id}/edit`)}
                onDelete={() => handleDelete(task.id)}
                onTogglePause={() => handleTogglePause(task)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

function TaskCard({
  task,
  onEdit,
  onDelete,
  onTogglePause,
}: Readonly<{
  task: ScheduledTask
  onEdit: () => void
  onDelete: () => void
  onTogglePause: () => void
}>) {
  return (
    <div className="flex flex-col rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 p-4 hover:border-zinc-300 dark:hover:border-zinc-600 transition-colors">
      <div className="flex items-start gap-3 mb-3">
        <div className="flex h-8 w-8 items-center justify-center rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 shrink-0">
          <CalendarClock className="h-4 w-4" />
        </div>
        <div className="flex-1 min-w-0">
          <h3 className="font-semibold text-sm text-zinc-900 dark:text-zinc-100 truncate">
            {task.name}
          </h3>
          <p className="text-xs text-zinc-400 dark:text-zinc-500 font-mono">
            {formatSchedule(task)}
          </p>
        </div>
        <StatusBadge status={task.status} />
      </div>

      {task.description && (
        <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-3 line-clamp-2 flex-1 leading-relaxed">
          {task.description}
        </p>
      )}

      <div className="flex flex-wrap gap-2 mb-3 text-xs text-zinc-500 dark:text-zinc-400">
        {task.agent_slug && (
          <span className="inline-flex items-center rounded-md bg-zinc-100 dark:bg-zinc-800 px-2 py-0.5 font-mono">
            {task.agent_slug}
          </span>
        )}
        <span>Runs: {task.run_count}</span>
        {task.last_run_at && <span>Last: {new Date(task.last_run_at).toLocaleString()}</span>}
        {task.last_run_status && <RunStatusBadge status={task.last_run_status} />}
      </div>

      <div className="flex items-center gap-1 pt-2 border-t border-zinc-100 dark:border-zinc-800">
        <button
          onClick={onTogglePause}
          className="flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-900 dark:hover:text-zinc-100 transition-colors"
        >
          {task.status === 'active' ? (
            <>
              <Pause className="h-3 w-3" /> Pause
            </>
          ) : (
            <>
              <Play className="h-3 w-3" /> Resume
            </>
          )}
        </button>
        <button
          onClick={onEdit}
          className="flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-900 dark:hover:text-zinc-100 transition-colors"
        >
          <Pencil className="h-3 w-3" />
          Edit
        </button>
        <AlertDialog>
          <AlertDialogTrigger asChild>
            <button className="flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs text-zinc-400 dark:text-zinc-500 hover:bg-red-50 dark:hover:bg-red-900/20 hover:text-red-600 dark:hover:text-red-400 transition-colors">
              <Trash2 className="h-3 w-3" />
              Delete
            </button>
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Delete task?</AlertDialogTitle>
              <AlertDialogDescription>
                This will permanently delete <strong>{task.name}</strong> and all its job history.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction
                onClick={onDelete}
                className="bg-red-600 text-white hover:bg-red-700"
              >
                Delete
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </div>
    </div>
  )
}
