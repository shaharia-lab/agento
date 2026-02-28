package scheduler

import (
	"context"
	"fmt"
	"time"

	"github.com/shaharia-lab/agento/internal/agent"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/storage"
)

// executeTask runs a single task execution with concurrency limiting.
func (s *Scheduler) executeTask(taskID string) {
	// Acquire semaphore.
	s.semaphore <- struct{}{}
	defer func() { <-s.semaphore }()

	task, err := s.cfg.TaskStore.GetTask(taskID)
	if err != nil {
		s.logger.Error("failed to load task for execution",
			"task_id", taskID, "error", err)
		return
	}
	if task == nil || task.Status != storage.TaskStatusActive {
		return
	}

	if s.shouldAutoPause(task) {
		return
	}

	s.logger.Info("executing task",
		"task_id", task.ID, "task_name", task.Name,
		"run_count", task.RunCount+1)

	s.runTask(task)
}

// shouldAutoPause checks stop conditions and pauses the task if met.
func (s *Scheduler) shouldAutoPause(task *storage.ScheduledTask) bool {
	if task.StopAfterCount > 0 && task.RunCount >= task.StopAfterCount {
		s.autoPause(task, "stop_after_count reached")
		return true
	}
	if task.StopAfterTime != nil && time.Now().After(*task.StopAfterTime) {
		s.autoPause(task, "stop_after_time reached")
		return true
	}
	return false
}

// runTask performs the core task execution: prompt interpolation, session
// creation, agent invocation, and result recording.
func (s *Scheduler) runTask(task *storage.ScheduledTask) {
	startedAt := time.Now().UTC()

	prompt, err := agent.Interpolate(task.Prompt, nil)
	if err != nil {
		s.logger.Error("failed to interpolate prompt",
			"task_id", task.ID, "error", err)
		s.recordFailedRun(task, startedAt, "",
			fmt.Sprintf("prompt interpolation: %v", err))
		return
	}

	chatSession, err := s.createTaskSession(task)
	if err != nil {
		s.logger.Error("failed to create chat session",
			"task_id", task.ID, "error", err)
		s.recordFailedRun(task, startedAt, "",
			fmt.Sprintf("create session: %v", err))
		return
	}

	jh := s.createInitialJobHistory(task, startedAt, chatSession.ID, prompt)

	agentCfg, err := s.resolveAgentConfig(task)
	if err != nil {
		s.logger.Error("failed to resolve agent config",
			"task_id", task.ID, "error", err)
		s.finishJobHistory(jh, startedAt, storage.JobStatusFailed,
			fmt.Sprintf("resolve agent: %v", err), agent.UsageStats{})
		s.updateTaskAfterRun(task, startedAt, "failed")
		return
	}

	opts := s.buildRunOptions(task)

	timeout := time.Duration(task.TimeoutMinutes) * time.Minute
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()

	result, err := agent.RunAgent(ctx, agentCfg, prompt, opts)
	if err != nil {
		s.logger.Error("task execution failed",
			"task_id", task.ID, "error", err)
		s.finishJobHistory(jh, startedAt, storage.JobStatusFailed,
			err.Error(), agent.UsageStats{})
		s.updateTaskAfterRun(task, startedAt, "failed")
		return
	}

	s.saveSessionResults(chatSession, result, prompt, startedAt)
	s.finishJobHistory(
		jh, startedAt, storage.JobStatusSuccess, "", result.Usage,
	)
	s.updateTaskAfterRun(task, startedAt, "success")

	s.logger.Info("task execution completed",
		"task_id", task.ID, "task_name", task.Name,
		"session_id", chatSession.ID, "run_count", task.RunCount+1)
}

// createTaskSession creates a chat session for the task execution.
func (s *Scheduler) createTaskSession(
	task *storage.ScheduledTask,
) (*storage.ChatSession, error) {
	chatSession, err := s.cfg.ChatStore.CreateSession(
		task.AgentSlug, task.WorkingDirectory,
		task.Model, task.SettingsProfileID,
	)
	if err != nil {
		return nil, err
	}

	chatSession.Title = "[Task] " + task.Name
	if updateErr := s.cfg.ChatStore.UpdateSession(chatSession); updateErr != nil {
		s.logger.Warn("failed to update session title", "error", updateErr)
	}
	return chatSession, nil
}

// createInitialJobHistory creates and persists an initial job history record.
func (s *Scheduler) createInitialJobHistory(
	task *storage.ScheduledTask, startedAt time.Time,
	chatSessionID, prompt string,
) *storage.JobHistory {
	promptPreview := prompt
	if len(promptPreview) > 200 {
		promptPreview = promptPreview[:200] + "..."
	}
	jh := &storage.JobHistory{
		TaskID:        task.ID,
		TaskName:      task.Name,
		AgentSlug:     task.AgentSlug,
		Status:        storage.JobStatusRunning,
		StartedAt:     startedAt,
		ChatSessionID: chatSessionID,
		Model:         task.Model,
		PromptPreview: promptPreview,
	}
	if err := s.cfg.TaskStore.CreateJobHistory(jh); err != nil {
		s.logger.Error("failed to create job history",
			"task_id", task.ID, "error", err)
	}
	return jh
}

// buildRunOptions constructs the agent RunOptions for a task.
func (s *Scheduler) buildRunOptions(task *storage.ScheduledTask) agent.RunOptions {
	opts := agent.RunOptions{
		LocalToolsMCP:       s.cfg.LocalMCP,
		MCPRegistry:         s.cfg.MCPRegistry,
		IntegrationRegistry: s.cfg.IntegrationRegistry,
	}

	if task.SettingsProfileID != "" {
		filePath, err := config.LoadProfileFilePath(task.SettingsProfileID)
		if err != nil {
			s.logger.Warn("failed to resolve settings profile", "error", err)
		} else {
			opts.SettingsFilePath = filePath
		}
	}
	return opts
}

// saveSessionResults updates the chat session with agent results and stores messages.
func (s *Scheduler) saveSessionResults(
	chatSession *storage.ChatSession, result *agent.AgentResult,
	prompt string, startedAt time.Time,
) {
	chatSession.SDKSession = result.SessionID
	chatSession.TotalInputTokens = result.Usage.InputTokens
	chatSession.TotalOutputTokens = result.Usage.OutputTokens
	chatSession.TotalCacheCreationTokens = result.Usage.CacheCreationInputTokens
	chatSession.TotalCacheReadTokens = result.Usage.CacheReadInputTokens
	chatSession.UpdatedAt = time.Now().UTC()
	if updateErr := s.cfg.ChatStore.UpdateSession(chatSession); updateErr != nil {
		s.logger.Warn("failed to update chat session after execution",
			"error", updateErr)
	}

	if result.Answer != "" {
		msg := storage.ChatMessage{
			Role:      "user",
			Content:   prompt,
			Timestamp: startedAt,
		}
		if appendErr := s.cfg.ChatStore.AppendMessage(chatSession.ID, msg); appendErr != nil {
			s.logger.Warn("failed to store user message", "error", appendErr)
		}

		assistantMsg := storage.ChatMessage{
			Role:      "assistant",
			Content:   result.Answer,
			Timestamp: time.Now().UTC(),
		}
		if appendErr := s.cfg.ChatStore.AppendMessage(chatSession.ID, assistantMsg); appendErr != nil {
			s.logger.Warn("failed to store assistant message", "error", appendErr)
		}
	}
}

func (s *Scheduler) resolveAgentConfig(task *storage.ScheduledTask) (*config.AgentConfig, error) {
	if task.AgentSlug != "" {
		agentCfg, err := s.cfg.AgentStore.Get(task.AgentSlug)
		if err != nil {
			return nil, fmt.Errorf("loading agent %q: %w", task.AgentSlug, err)
		}
		if agentCfg == nil {
			return nil, fmt.Errorf("agent %q not found", task.AgentSlug)
		}
		return agentCfg, nil
	}

	// Synthesize minimal config — use the user's configured default model from settings.
	model := task.Model
	if model == "" && s.cfg.SettingsManager != nil {
		model = s.cfg.SettingsManager.Get().DefaultModel
	}
	return &config.AgentConfig{
		Model:    model,
		Thinking: "adaptive",
	}, nil
}

func (s *Scheduler) finishJobHistory(
	jh *storage.JobHistory, startedAt time.Time,
	status storage.JobStatus, errMsg string, usage agent.UsageStats,
) {
	now := time.Now().UTC()
	jh.Status = status
	jh.FinishedAt = &now
	jh.DurationMS = now.Sub(startedAt).Milliseconds()
	jh.ErrorMessage = errMsg
	jh.TotalInputTokens = usage.InputTokens
	jh.TotalOutputTokens = usage.OutputTokens
	jh.TotalCacheCreationTokens = usage.CacheCreationInputTokens
	jh.TotalCacheReadTokens = usage.CacheReadInputTokens

	if err := s.cfg.TaskStore.UpdateJobHistory(jh); err != nil {
		s.logger.Error("failed to update job history", "job_id", jh.ID, "error", err)
	}
}

func (s *Scheduler) updateTaskAfterRun(task *storage.ScheduledTask, ranAt time.Time, status string) {
	task.RunCount++
	task.LastRunAt = &ranAt
	task.LastRunStatus = status

	// Auto-pause one-off tasks after execution.
	if task.ScheduleType == storage.ScheduleOneOff {
		task.Status = storage.TaskStatusPaused
		s.UnscheduleTask(task.ID)
	}

	// Check if stop conditions are now met.
	if task.StopAfterCount > 0 && task.RunCount >= task.StopAfterCount {
		task.Status = storage.TaskStatusPaused
		s.UnscheduleTask(task.ID)
	}

	if err := s.cfg.TaskStore.UpdateTask(task); err != nil {
		s.logger.Error("failed to update task after run", "task_id", task.ID, "error", err)
	}
}

func (s *Scheduler) recordFailedRun(task *storage.ScheduledTask, startedAt time.Time, chatSessionID, errMsg string) {
	jh := &storage.JobHistory{
		TaskID:        task.ID,
		TaskName:      task.Name,
		AgentSlug:     task.AgentSlug,
		Status:        storage.JobStatusFailed,
		StartedAt:     startedAt,
		ChatSessionID: chatSessionID,
		ErrorMessage:  errMsg,
	}
	now := time.Now().UTC()
	jh.FinishedAt = &now
	jh.DurationMS = now.Sub(startedAt).Milliseconds()

	if err := s.cfg.TaskStore.CreateJobHistory(jh); err != nil {
		s.logger.Error("failed to create failed job history", "task_id", task.ID, "error", err)
	}
	s.updateTaskAfterRun(task, startedAt, "failed")
}

func (s *Scheduler) autoPause(task *storage.ScheduledTask, reason string) {
	s.logger.Info("auto-pausing task", "task_id", task.ID, "reason", reason)
	task.Status = storage.TaskStatusPaused
	if err := s.cfg.TaskStore.UpdateTask(task); err != nil {
		s.logger.Error("failed to auto-pause task", "task_id", task.ID, "error", err)
	}
	s.UnscheduleTask(task.ID)
}
