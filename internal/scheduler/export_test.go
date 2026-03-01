package scheduler

// ExportedExecuteTask exposes the private executeTask method for external tests.
func (s *Scheduler) ExportedExecuteTask(taskID string) {
	s.executeTask(taskID)
}
