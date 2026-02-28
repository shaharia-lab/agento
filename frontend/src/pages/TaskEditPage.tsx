import { useState, useEffect } from 'react'
import { useParams } from 'react-router-dom'
import { tasksApi } from '@/lib/api'
import type { ScheduledTask } from '@/types'
import TaskForm from '@/components/TaskForm'

export default function TaskEditPage() {
  const { id } = useParams<{ id: string }>()
  const [task, setTask] = useState<ScheduledTask | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!id) return
    tasksApi
      .get(id)
      .then(setTask)
      .catch(err => setError(err instanceof Error ? err.message : 'Failed to load task'))
      .finally(() => setLoading(false))
  }, [id])

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400 dark:text-zinc-500">Loading task…</div>
      </div>
    )
  }

  if (error || !task) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-red-500">{error ?? 'Task not found'}</div>
      </div>
    )
  }

  return <TaskForm initialData={task} isEdit />
}
