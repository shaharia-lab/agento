package scheduler

import (
	"context"
	"fmt"
	"strconv"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/trace"

	"github.com/shaharia-lab/agento/internal/agent"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/storage"
)

// executeTask runs a single task execution with concurrency limiting.
func (s *Scheduler) executeTask(taskID string) {
	// Acquire semaphore.
	s.semaphore <- struct{}{}
	defer func() { <-s.semaphore }()

	// Root span for this task execution. Using context.Background() because
	// scheduled tasks are not triggered by an HTTP request — they start a new
	// trace rooted here.
	ctx, span := otel.Tracer("agento").Start(context.Background(), "scheduler.task.execute")
	span.SetAttributes(attribute.String("scheduler.task_id", taskID))
	defer span.End()

	task, err := s.cfg.TaskStore.GetTask(ctx, taskID)
	if err != nil {
		s.logger.Error("failed to load task for execution",
			"task_id", taskID, "error", err)
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return
	}
	if task == nil || task.Status != storage.TaskStatusActive {
		return
	}

	span.SetAttributes(
		attribute.String("scheduler.task_name", task.Name),
		attribute.String("scheduler.agent_slug", task.AgentSlug),
		attribute.String("scheduler.trigger", "scheduled"),
	)

	if s.shouldAutoPause(ctx, task) {
		return
	}

	s.logger.Info("executing task",
		"task_id", task.ID, "task_name", task.Name,
		"run_count", task.RunCount+1)

	s.runTask(ctx, task, span)
}

// shouldAutoPause checks stop conditions and pauses the task if met.
func (s *Scheduler) shouldAutoPause(ctx context.Context, task *storage.ScheduledTask) bool {
	if task.StopAfterCount > 0 && task.RunCount >= task.StopAfterCount {
		s.autoPause(ctx, task, "stop_after_count reached")
		return true
	}
	if task.StopAfterTime != nil && time.Now().After(*task.StopAfterTime) {
		s.autoPause(ctx, task, "stop_after_time reached")
		return true
	}
	return false
}

// prepareTaskRun interpolates the prompt and creates the chat session and
// initial job history record. On any failure it records the failed run,
// publishes the failed event, and returns a non-nil error.
func (s *Scheduler) prepareTaskRun(
	ctx context.Context, task *storage.ScheduledTask, startedAt time.Time,
) (prompt string, chatSession *storage.ChatSession, jh *storage.JobHistory, err error) {
	prompt, err = agent.Interpolate(task.Prompt, nil)
	if err != nil {
		errMsg := fmt.Sprintf("prompt interpolation: %v", err)
		s.logger.Error("failed to interpolate prompt", "task_id", task.ID, "error", err)
		s.recordFailedRun(ctx, task, startedAt, "", errMsg)
		s.publishTaskFailed(task, errMsg)
		return "", nil, nil, err
	}

	chatSession, err = s.createTaskSession(ctx, task)
	if err != nil {
		errMsg := fmt.Sprintf("create session: %v", err)
		s.logger.Error("failed to create chat session", "task_id", task.ID, "error", err)
		s.recordFailedRun(ctx, task, startedAt, "", errMsg)
		s.publishTaskFailed(task, errMsg)
		return "", nil, nil, err
	}

	jh = s.createInitialJobHistory(ctx, task, startedAt, chatSession.ID, prompt)
	return prompt, chatSession, jh, nil
}

// runTask performs the core task execution: prompt interpolation, session
// creation, agent invocation, and result recording.
// parentCtx carries the root trace span from executeTask.
func (s *Scheduler) runTask(parentCtx context.Context, task *storage.ScheduledTask, parentSpan trace.Span) {
	startedAt := time.Now().UTC()

	prompt, chatSession, jh, err := s.prepareTaskRun(parentCtx, task, startedAt)
	if err != nil {
		parentSpan.RecordError(err)
		parentSpan.SetStatus(codes.Error, err.Error())
		return
	}

	agentCfg, err := s.resolveAgentConfig(parentCtx, task)
	if err != nil {
		errMsg := fmt.Sprintf("resolve agent: %v", err)
		s.logger.Error("failed to resolve agent config",
			"task_id", task.ID, "error", err)
		s.finishJobHistory(parentCtx, jh, startedAt, storage.JobStatusFailed,
			errMsg, agent.UsageStats{}, "")
		s.updateTaskAfterRun(parentCtx, task, startedAt, "failed")
		s.publishTaskFailed(task, errMsg)
		parentSpan.RecordError(err)
		parentSpan.SetStatus(codes.Error, errMsg)
		return
	}

	opts := s.buildRunOptions(task)

	timeout := time.Duration(task.TimeoutMinutes) * time.Minute
	ctx, cancel := context.WithTimeout(parentCtx, timeout)
	defer cancel()

	result, err := agent.RunAgent(ctx, agentCfg, prompt, opts)
	if err != nil {
		s.logger.Error("task execution failed",
			"task_id", task.ID, "error", err)
		s.finishJobHistory(parentCtx, jh, startedAt, storage.JobStatusFailed,
			err.Error(), agent.UsageStats{}, "")
		s.updateTaskAfterRun(parentCtx, task, startedAt, "failed")
		s.publishTaskFailed(task, err.Error())
		parentSpan.RecordError(err)
		parentSpan.SetStatus(codes.Error, err.Error())
		return
	}

	s.saveSessionResults(parentCtx, chatSession, result, prompt, startedAt)
	responseText := ""
	if task.SaveOutput {
		responseText = result.Answer
	}
	s.finishJobHistory(
		parentCtx, jh, startedAt, storage.JobStatusSuccess, "", result.Usage, responseText,
	)
	s.updateTaskAfterRun(parentCtx, task, startedAt, "success")
	s.publishTaskFinished(task, jh, chatSession.ID)

	s.logger.Info("task execution completed",
		"task_id", task.ID, "task_name", task.Name,
		"session_id", chatSession.ID, "run_count", task.RunCount)
}

// createTaskSession creates a chat session for the task execution.
func (s *Scheduler) createTaskSession(
	ctx context.Context, task *storage.ScheduledTask,
) (*storage.ChatSession, error) {
	chatSession, err := s.cfg.ChatStore.CreateSession(
		ctx,
		task.AgentSlug, task.WorkingDirectory,
		task.Model, task.SettingsProfileID,
	)
	if err != nil {
		return nil, err
	}

	chatSession.Title = "[Task] " + task.Name
	if updateErr := s.cfg.ChatStore.UpdateSession(ctx, chatSession); updateErr != nil {
		s.logger.Warn("failed to update session title", "error", updateErr)
	}
	return chatSession, nil
}

// createInitialJobHistory creates and persists an initial job history record.
func (s *Scheduler) createInitialJobHistory(
	ctx context.Context, task *storage.ScheduledTask, startedAt time.Time,
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
	if err := s.cfg.TaskStore.CreateJobHistory(ctx, jh); err != nil {
		s.logger.Error("failed to create job history",
			"task_id", task.ID, "error", err)
	}
	return jh
}

// buildRunOptions constructs the agent RunOptions for a task.
func (s *Scheduler) buildRunOptions(task *storage.ScheduledTask) agent.RunOptions {
	return agent.RunOptions{
		LocalToolsMCP:       s.cfg.LocalMCP,
		MCPRegistry:         s.cfg.MCPRegistry,
		IntegrationRegistry: s.cfg.IntegrationRegistry,
		WorkingDir:          task.WorkingDirectory,
		// Resolved to a path inside the runner, which knows the agent's
		// Claude config dir; an empty ID means the dir's own settings.json.
		SettingsProfileID: task.SettingsProfileID,
	}
}

// saveSessionResults updates the chat session with agent results and stores messages.
func (s *Scheduler) saveSessionResults(
	ctx context.Context, chatSession *storage.ChatSession, result *agent.AgentResult,
	prompt string, startedAt time.Time,
) {
	chatSession.SDKSession = result.SessionID
	chatSession.TotalInputTokens = result.Usage.InputTokens
	chatSession.TotalOutputTokens = result.Usage.OutputTokens
	chatSession.TotalCacheCreationTokens = result.Usage.CacheCreationInputTokens
	chatSession.TotalCacheReadTokens = result.Usage.CacheReadInputTokens
	chatSession.UpdatedAt = time.Now().UTC()
	if updateErr := s.cfg.ChatStore.UpdateSession(ctx, chatSession); updateErr != nil {
		s.logger.Warn("failed to update chat session after execution",
			"error", updateErr)
	}

	if result.Answer != "" {
		msg := storage.ChatMessage{
			Role:      "user",
			Content:   prompt,
			Timestamp: startedAt,
		}
		if appendErr := s.cfg.ChatStore.AppendMessage(ctx, chatSession.ID, msg); appendErr != nil {
			s.logger.Warn("failed to store user message", "error", appendErr)
		}

		assistantMsg := storage.ChatMessage{
			Role:      "assistant",
			Content:   result.Answer,
			Timestamp: time.Now().UTC(),
		}
		if appendErr := s.cfg.ChatStore.AppendMessage(ctx, chatSession.ID, assistantMsg); appendErr != nil {
			s.logger.Warn("failed to store assistant message", "error", appendErr)
		}
	}
}

func (s *Scheduler) resolveAgentConfig(ctx context.Context, task *storage.ScheduledTask) (*config.AgentConfig, error) {
	if task.AgentSlug != "" {
		agentCfg, err := s.cfg.AgentStore.Get(ctx, task.AgentSlug)
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
	ctx context.Context, jh *storage.JobHistory, startedAt time.Time,
	status storage.JobStatus, errMsg string, usage agent.UsageStats,
	responseText string,
) {
	now := time.Now().UTC()
	jh.Status = status
	jh.FinishedAt = &now
	jh.DurationMS = now.Sub(startedAt).Milliseconds()
	jh.ErrorMessage = errMsg
	jh.ResponseText = responseText
	jh.TotalInputTokens = usage.InputTokens
	jh.TotalOutputTokens = usage.OutputTokens
	jh.TotalCacheCreationTokens = usage.CacheCreationInputTokens
	jh.TotalCacheReadTokens = usage.CacheReadInputTokens

	if err := s.cfg.TaskStore.UpdateJobHistory(ctx, jh); err != nil {
		s.logger.Error("failed to update job history", "job_id", jh.ID, "error", err)
	}
}

func (s *Scheduler) updateTaskAfterRun(
	ctx context.Context, task *storage.ScheduledTask, ranAt time.Time, status string,
) {
	task.RunCount++
	task.LastRunAt = &ranAt
	task.LastRunStatus = status

	// Auto-pause one-time tasks after execution so they don't re-run on restart.
	if task.ScheduleType == storage.ScheduleOneOff || task.ScheduleType == storage.ScheduleRunImmediately {
		task.Status = storage.TaskStatusPaused
		s.UnscheduleTask(task.ID)
	}

	// Check if stop conditions are now met.
	if task.StopAfterCount > 0 && task.RunCount >= task.StopAfterCount {
		task.Status = storage.TaskStatusPaused
		s.UnscheduleTask(task.ID)
	}

	if err := s.cfg.TaskStore.UpdateTask(ctx, task); err != nil {
		s.logger.Error("failed to update task after run", "task_id", task.ID, "error", err)
	}
}

func (s *Scheduler) recordFailedRun(
	ctx context.Context, task *storage.ScheduledTask, startedAt time.Time, chatSessionID, errMsg string,
) {
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

	if err := s.cfg.TaskStore.CreateJobHistory(ctx, jh); err != nil {
		s.logger.Error("failed to create failed job history", "task_id", task.ID, "error", err)
	}
	s.updateTaskAfterRun(ctx, task, startedAt, "failed")
}

func (s *Scheduler) autoPause(ctx context.Context, task *storage.ScheduledTask, reason string) {
	s.logger.Info("auto-pausing task", "task_id", task.ID, "reason", reason)
	task.Status = storage.TaskStatusPaused
	if err := s.cfg.TaskStore.UpdateTask(ctx, task); err != nil {
		s.logger.Error("failed to auto-pause task", "task_id", task.ID, "error", err)
	}
	s.UnscheduleTask(task.ID)
}

// publishTaskFinished publishes a task-finished event with execution details.
func (s *Scheduler) publishTaskFinished(
	task *storage.ScheduledTask, jh *storage.JobHistory, chatSessionID string,
) {
	if s.cfg.EventPublisher == nil {
		return
	}
	s.cfg.EventPublisher.Publish(EventTaskFinished, map[string]string{
		"Task ID":          task.ID,
		"Task Name":        task.Name,
		"Task Description": task.Description,
		"Agent":            task.AgentSlug,
		"Status":           "Completed successfully",
		"Duration":         strconv.FormatInt(jh.DurationMS, 10) + " ms",
		"Run Count":        strconv.Itoa(task.RunCount),
		"Model":            jh.Model,
		"Chat Session ID":  chatSessionID,
	})
}

// publishTaskFailed publishes a task-failed event with the error details.
func (s *Scheduler) publishTaskFailed(task *storage.ScheduledTask, errMsg string) {
	if s.cfg.EventPublisher == nil {
		return
	}
	s.cfg.EventPublisher.Publish(EventTaskFailed, map[string]string{
		"Task ID":          task.ID,
		"Task Name":        task.Name,
		"Task Description": task.Description,
		"Agent":            task.AgentSlug,
		"Status":           "Failed",
		"Error":            errMsg,
		"Run Count":        strconv.Itoa(task.RunCount),
	})
}
