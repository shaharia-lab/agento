package scheduler

import (
	"context"
	"fmt"
	"log/slog"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/go-co-op/gocron/v2"
	"github.com/google/uuid"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/integrations"
	"github.com/shaharia-lab/agento/internal/storage"
	"github.com/shaharia-lab/agento/internal/tools"
)

// EventPublisher allows the scheduler to emit events without depending on a
// concrete event bus implementation.
type EventPublisher interface {
	Publish(eventType string, payload map[string]string)
}

// Event type constants for task lifecycle notifications.
const (
	EventTaskFinished = "tasks_scheduler.task_execution.finished"
	EventTaskFailed   = "tasks_scheduler.task_execution.failed"
)

// Config holds the scheduler configuration.
type Config struct {
	TaskStore           storage.TaskStore
	ChatStore           storage.ChatStore
	AgentStore          storage.AgentStore
	MCPRegistry         *config.MCPRegistry
	LocalMCP            *tools.LocalMCPConfig
	IntegrationRegistry *integrations.IntegrationRegistry
	SettingsManager     *config.SettingsManager
	Logger              *slog.Logger
	MaxConcurrency      int
	// EventPublisher is optional. When set, task lifecycle events are published.
	EventPublisher EventPublisher
}

// Scheduler manages scheduled task execution using gocron.
type Scheduler struct {
	cron      gocron.Scheduler
	cfg       Config
	jobs      map[string]uuid.UUID // taskID → gocron job UUID
	mu        sync.Mutex
	semaphore chan struct{}
	logger    *slog.Logger
}

// New creates a new Scheduler.
func New(cfg Config) (*Scheduler, error) {
	cron, err := gocron.NewScheduler()
	if err != nil {
		return nil, fmt.Errorf("creating gocron scheduler: %w", err)
	}

	maxConc := cfg.MaxConcurrency
	if maxConc <= 0 {
		maxConc = 3
	}

	return &Scheduler{
		cron:      cron,
		cfg:       cfg,
		jobs:      make(map[string]uuid.UUID),
		semaphore: make(chan struct{}, maxConc),
		logger:    cfg.Logger,
	}, nil
}

// Start loads active tasks from the database, schedules them, and starts the gocron scheduler.
func (s *Scheduler) Start(_ context.Context) error {
	tasks, err := s.cfg.TaskStore.ListTasks()
	if err != nil {
		return fmt.Errorf("loading tasks: %w", err)
	}

	for _, task := range tasks {
		if task.Status != storage.TaskStatusActive {
			continue
		}
		if err := s.ScheduleTask(task); err != nil {
			s.logger.Warn("failed to schedule task on startup",
				"task_id", task.ID, "task_name", task.Name, "error", err)
		}
	}

	s.cron.Start()
	s.logger.Info("task scheduler started", "active_tasks", len(s.jobs))
	return nil
}

// Stop shuts down the gocron scheduler.
func (s *Scheduler) Stop() error {
	return s.cron.Shutdown()
}

// ScheduleTask adds or replaces a task's schedule in gocron.
func (s *Scheduler) ScheduleTask(task *storage.ScheduledTask) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	// Remove existing job if any.
	if jobID, ok := s.jobs[task.ID]; ok {
		if err := s.cron.RemoveJob(jobID); err != nil {
			s.logger.Warn("failed to remove existing job", "task_id", task.ID, "error", err)
		}
		delete(s.jobs, task.ID)
	}

	jobDef, err := s.buildJobDefinition(task)
	if err != nil {
		return fmt.Errorf("building job definition for task %q: %w", task.ID, err)
	}

	taskID := task.ID
	job, err := s.cron.NewJob(jobDef, gocron.NewTask(func() {
		s.executeTask(taskID)
	}))
	if err != nil {
		return fmt.Errorf("scheduling task %q: %w", task.ID, err)
	}

	s.jobs[task.ID] = job.ID()
	s.logger.Info("task scheduled", "task_id", task.ID, "task_name", task.Name,
		"schedule_type", task.ScheduleType)
	return nil
}

// UnscheduleTask removes a task from the gocron scheduler.
func (s *Scheduler) UnscheduleTask(taskID string) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if jobID, ok := s.jobs[taskID]; ok {
		if err := s.cron.RemoveJob(jobID); err != nil {
			s.logger.Warn("failed to remove job", "task_id", taskID, "error", err)
		}
		delete(s.jobs, taskID)
		s.logger.Info("task unscheduled", "task_id", taskID)
	}
}

// buildJobDefinition converts a ScheduledTask's schedule config into a gocron JobDefinition.
func (s *Scheduler) buildJobDefinition(task *storage.ScheduledTask) (gocron.JobDefinition, error) {
	cfg := task.ScheduleConfig

	switch task.ScheduleType {
	case storage.ScheduleOneOff:
		runAt, err := time.Parse(time.RFC3339, cfg.RunAt)
		if err != nil {
			return nil, fmt.Errorf("parsing run_at time: %w", err)
		}
		return gocron.OneTimeJob(gocron.OneTimeJobStartDateTime(runAt)), nil

	case storage.ScheduleInterval:
		return s.buildIntervalJob(cfg)

	case storage.ScheduleCron:
		return gocron.CronJob(cfg.Expression, false), nil

	default:
		return nil, fmt.Errorf("unknown schedule type: %s", task.ScheduleType)
	}
}

// buildIntervalJob creates a gocron job for interval-based schedules.
func (s *Scheduler) buildIntervalJob(cfg storage.ScheduleConfig) (gocron.JobDefinition, error) {
	if cfg.EveryMinutes > 0 {
		return gocron.DurationJob(time.Duration(cfg.EveryMinutes) * time.Minute), nil
	}
	if cfg.EveryHours > 0 {
		return gocron.DurationJob(time.Duration(cfg.EveryHours) * time.Hour), nil
	}
	if cfg.EveryDays > 0 {
		if cfg.AtTime != "" {
			if def, err := s.buildDailyAtTimeJob(cfg); err == nil {
				return def, nil
			}
		}
		return gocron.DurationJob(time.Duration(cfg.EveryDays) * 24 * time.Hour), nil
	}
	return nil, fmt.Errorf("invalid interval config")
}

// buildDailyAtTimeJob parses an "HH:MM" at_time string and returns a DailyJob definition.
func (s *Scheduler) buildDailyAtTimeJob(cfg storage.ScheduleConfig) (gocron.JobDefinition, error) {
	parts := strings.Split(cfg.AtTime, ":")
	if len(parts) != 2 {
		return nil, fmt.Errorf("invalid at_time format: %s", cfg.AtTime)
	}
	hour, err := strconv.Atoi(parts[0])
	if err != nil {
		return nil, fmt.Errorf("parsing hour from at_time: %w", err)
	}
	minute, err := strconv.Atoi(parts[1])
	if err != nil {
		return nil, fmt.Errorf("parsing minute from at_time: %w", err)
	}
	if hour < 0 || hour > 23 || minute < 0 || minute > 59 {
		return nil, fmt.Errorf("at_time values out of range: %d:%d", hour, minute)
	}
	if cfg.EveryDays < 0 {
		return nil, fmt.Errorf("every_days must be positive, got %d", cfg.EveryDays)
	}
	return gocron.DailyJob(
		uint(cfg.EveryDays), //nolint:gosec // bounds checked above
		gocron.NewAtTimes(gocron.NewAtTime(
			uint(hour),   //nolint:gosec // bounds checked above
			uint(minute), //nolint:gosec // bounds checked above
			0,
		)),
	), nil
}
