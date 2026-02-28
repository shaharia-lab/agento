package service

import (
	"context"
	"fmt"
	"log/slog"

	"github.com/shaharia-lab/agento/internal/storage"
)

// TaskService defines the business logic interface for managing scheduled tasks.
type TaskService interface {
	ListTasks(ctx context.Context) ([]*storage.ScheduledTask, error)
	GetTask(ctx context.Context, id string) (*storage.ScheduledTask, error)
	CreateTask(ctx context.Context, task *storage.ScheduledTask) (*storage.ScheduledTask, error)
	UpdateTask(ctx context.Context, id string, task *storage.ScheduledTask) (*storage.ScheduledTask, error)
	DeleteTask(ctx context.Context, id string) error
	PauseTask(ctx context.Context, id string) (*storage.ScheduledTask, error)
	ResumeTask(ctx context.Context, id string) (*storage.ScheduledTask, error)
	ListJobHistory(ctx context.Context, taskID string, limit int) ([]*storage.JobHistory, error)
	ListAllJobHistory(ctx context.Context, limit, offset int) ([]*storage.JobHistory, error)
	GetJobHistory(ctx context.Context, id string) (*storage.JobHistory, error)
}

type taskService struct {
	repo   storage.TaskStore
	logger *slog.Logger
}

// NewTaskService returns a new TaskService backed by the given TaskStore.
func NewTaskService(repo storage.TaskStore, logger *slog.Logger) TaskService {
	return &taskService{repo: repo, logger: logger}
}

func (s *taskService) ListTasks(_ context.Context) ([]*storage.ScheduledTask, error) {
	tasks, err := s.repo.ListTasks()
	if err != nil {
		return nil, fmt.Errorf("listing tasks: %w", err)
	}
	return tasks, nil
}

func (s *taskService) GetTask(_ context.Context, id string) (*storage.ScheduledTask, error) {
	task, err := s.repo.GetTask(id)
	if err != nil {
		return nil, fmt.Errorf("getting task %q: %w", id, err)
	}
	if task == nil {
		return nil, &NotFoundError{Resource: "task", ID: id}
	}
	return task, nil
}

func (s *taskService) CreateTask(_ context.Context, task *storage.ScheduledTask) (*storage.ScheduledTask, error) {
	if err := validateTask(task); err != nil {
		return nil, err
	}

	if task.Status == "" {
		task.Status = storage.TaskStatusActive
	}
	if task.TimeoutMinutes == 0 {
		task.TimeoutMinutes = 30
	}

	if err := s.repo.CreateTask(task); err != nil {
		return nil, fmt.Errorf("creating task: %w", err)
	}

	s.logger.Info("task created", "id", task.ID, "name", task.Name)
	return task, nil
}

func (s *taskService) UpdateTask(
	_ context.Context, id string, task *storage.ScheduledTask,
) (*storage.ScheduledTask, error) {
	existing, err := s.repo.GetTask(id)
	if err != nil {
		return nil, fmt.Errorf("looking up task: %w", err)
	}
	if existing == nil {
		return nil, &NotFoundError{Resource: "task", ID: id}
	}

	task.ID = id
	task.RunCount = existing.RunCount
	task.LastRunAt = existing.LastRunAt
	task.LastRunStatus = existing.LastRunStatus
	task.CreatedAt = existing.CreatedAt

	if err := validateTask(task); err != nil {
		return nil, err
	}

	if task.TimeoutMinutes == 0 {
		task.TimeoutMinutes = 30
	}

	if err := s.repo.UpdateTask(task); err != nil {
		return nil, fmt.Errorf("updating task: %w", err)
	}

	s.logger.Info("task updated", "id", id, "name", task.Name)
	return task, nil
}

func (s *taskService) DeleteTask(_ context.Context, id string) error {
	existing, err := s.repo.GetTask(id)
	if err != nil {
		return fmt.Errorf("looking up task: %w", err)
	}
	if existing == nil {
		return &NotFoundError{Resource: "task", ID: id}
	}

	if err := s.repo.DeleteTask(id); err != nil {
		return fmt.Errorf("deleting task %q: %w", id, err)
	}
	s.logger.Info("task deleted", "id", id)
	return nil
}

func (s *taskService) PauseTask(_ context.Context, id string) (*storage.ScheduledTask, error) {
	task, err := s.repo.GetTask(id)
	if err != nil {
		return nil, fmt.Errorf("looking up task: %w", err)
	}
	if task == nil {
		return nil, &NotFoundError{Resource: "task", ID: id}
	}

	task.Status = storage.TaskStatusPaused
	if err := s.repo.UpdateTask(task); err != nil {
		return nil, fmt.Errorf("pausing task: %w", err)
	}

	s.logger.Info("task paused", "id", id)
	return task, nil
}

func (s *taskService) ResumeTask(_ context.Context, id string) (*storage.ScheduledTask, error) {
	task, err := s.repo.GetTask(id)
	if err != nil {
		return nil, fmt.Errorf("looking up task: %w", err)
	}
	if task == nil {
		return nil, &NotFoundError{Resource: "task", ID: id}
	}

	task.Status = storage.TaskStatusActive
	task.RunCount = 0
	task.LastRunAt = nil
	task.LastRunStatus = ""
	if err := s.repo.UpdateTask(task); err != nil {
		return nil, fmt.Errorf("resuming task: %w", err)
	}

	s.logger.Info("task resumed", "id", id)
	return task, nil
}

func (s *taskService) ListJobHistory(_ context.Context, taskID string, limit int) ([]*storage.JobHistory, error) {
	if limit <= 0 {
		limit = 50
	}
	history, err := s.repo.ListJobHistory(taskID, limit)
	if err != nil {
		return nil, fmt.Errorf("listing job history: %w", err)
	}
	return history, nil
}

func (s *taskService) ListAllJobHistory(_ context.Context, limit, offset int) ([]*storage.JobHistory, error) {
	if limit <= 0 {
		limit = 50
	}
	if offset < 0 {
		offset = 0
	}
	history, err := s.repo.ListAllJobHistory(limit, offset)
	if err != nil {
		return nil, fmt.Errorf("listing all job history: %w", err)
	}
	return history, nil
}

func (s *taskService) GetJobHistory(_ context.Context, id string) (*storage.JobHistory, error) {
	jh, err := s.repo.GetJobHistory(id)
	if err != nil {
		return nil, fmt.Errorf("getting job history %q: %w", id, err)
	}
	if jh == nil {
		return nil, &NotFoundError{Resource: "job_history", ID: id}
	}
	return jh, nil
}

func validateTask(task *storage.ScheduledTask) error {
	if task.Name == "" {
		return &ValidationError{Field: "name", Message: "name is required"}
	}
	if task.Prompt == "" {
		return &ValidationError{Field: "prompt", Message: "prompt is required"}
	}
	if task.TimeoutMinutes < 0 || task.TimeoutMinutes > 240 {
		return &ValidationError{Field: "timeout_minutes", Message: "timeout must be between 1 and 240 minutes"}
	}

	switch task.ScheduleType {
	case storage.ScheduleOneOff, storage.ScheduleInterval, storage.ScheduleCron:
		// valid
	case "":
		task.ScheduleType = storage.ScheduleOneOff
	default:
		return &ValidationError{Field: "schedule_type", Message: "must be one_off, interval, or cron"}
	}

	return validateScheduleConfig(task)
}

func validateScheduleConfig(task *storage.ScheduledTask) error {
	cfg := task.ScheduleConfig
	switch task.ScheduleType {
	case storage.ScheduleOneOff:
		if cfg.RunAt == "" {
			return &ValidationError{Field: "schedule_config.run_at", Message: "run_at is required for one_off schedules"}
		}
	case storage.ScheduleInterval:
		if cfg.EveryMinutes == 0 && cfg.EveryHours == 0 && cfg.EveryDays == 0 {
			return &ValidationError{
				Field:   "schedule_config",
				Message: "at least one of every_minutes, every_hours, or every_days is required for interval schedules",
			}
		}
	case storage.ScheduleCron:
		if cfg.Expression == "" {
			return &ValidationError{Field: "schedule_config.expression", Message: "expression is required for cron schedules"}
		}
	}
	return nil
}
