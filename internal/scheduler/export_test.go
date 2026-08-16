package scheduler

// ExportedExecuteTask exposes the private executeTask method for external tests.
func (s *Scheduler) ExportedExecuteTask(taskID string) {
	s.executeTask(taskID)
}

// IsTaskScheduled reports whether a task has a live gocron job, which is
// otherwise only visible through the unexported jobs map.
func (s *Scheduler) IsTaskScheduled(taskID string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, ok := s.jobs[taskID]
	return ok
}
