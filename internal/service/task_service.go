package service

import (
	"context"
	"fmt"
	"log/slog"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/codes"

	"github.com/shaharia-lab/agento/internal/storage"
)

// TaskScheduler is the subset of the scheduler that the TaskService needs.
// Using an interface here prevents an import cycle: service must not import scheduler.
type TaskScheduler interface {
	ScheduleTask(task *storage.ScheduledTask) error
	UnscheduleTask(taskID string)
}

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
	DeleteJobHistory(ctx context.Context, id string) error
	BulkDeleteJobHistory(ctx context.Context, ids []string) error
}

const errFmtLookingUpTask = "looking up task: %w"

type taskService struct {
	repo      storage.TaskStore
	scheduler TaskScheduler // optional; nil if no scheduler is configured
	logger    *slog.Logger
}

// NewTaskService returns a new TaskService backed by the given TaskStore.
// scheduler may be nil when running without task scheduling (e.g. in tests).
func NewTaskService(repo storage.TaskStore, scheduler TaskScheduler, logger *slog.Logger) TaskService {
	return &taskService{repo: repo, scheduler: scheduler, logger: logger}
}

func (s *taskService) ListTasks(ctx context.Context) ([]*storage.ScheduledTask, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.list")
	defer span.End()

	tasks, err := s.repo.ListTasks(ctx)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("listing tasks: %w", err)
	}
	return tasks, nil
}

func (s *taskService) GetTask(ctx context.Context, id string) (*storage.ScheduledTask, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.get")
	defer span.End()

	task, err := s.repo.GetTask(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("getting task %q: %w", id, err)
	}
	if task == nil {
		return nil, &NotFoundError{Resource: "task", ID: id}
	}
	return task, nil
}

func (s *taskService) CreateTask(ctx context.Context, task *storage.ScheduledTask) (*storage.ScheduledTask, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.create")
	defer span.End()

	if err := validateTask(task); err != nil {
		return nil, err
	}

	if task.Status == "" {
		task.Status = storage.TaskStatusActive
	}
	if task.TimeoutMinutes == 0 {
		task.TimeoutMinutes = 30
	}

	if err := s.repo.CreateTask(ctx, task); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("creating task: %w", err)
	}

	s.logger.Info("task created", "id", task.ID, "name", task.Name)

	// Schedule the task if a scheduler is configured and the task is active.
	if s.scheduler != nil && task.Status == storage.TaskStatusActive {
		if schedErr := s.scheduler.ScheduleTask(task); schedErr != nil {
			s.logger.Warn("failed to schedule newly created task", "task_id", task.ID, "error", schedErr)
		}
	}

	return task, nil
}

func (s *taskService) UpdateTask(
	ctx context.Context, id string, task *storage.ScheduledTask,
) (*storage.ScheduledTask, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.update")
	defer span.End()

	existing, err := s.repo.GetTask(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf(errFmtLookingUpTask, err)
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

	if err := s.repo.UpdateTask(ctx, task); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("updating task: %w", err)
	}

	s.logger.Info("task updated", "id", id, "name", task.Name)

	// Reschedule: always unschedule first, then reschedule if still active.
	if s.scheduler != nil {
		s.scheduler.UnscheduleTask(id)
		if task.Status == storage.TaskStatusActive {
			if schedErr := s.scheduler.ScheduleTask(task); schedErr != nil {
				s.logger.Warn("failed to reschedule updated task", "task_id", id, "error", schedErr)
			}
		}
	}

	return task, nil
}

func (s *taskService) DeleteTask(ctx context.Context, id string) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.delete")
	defer span.End()

	existing, err := s.repo.GetTask(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf(errFmtLookingUpTask, err)
	}
	if existing == nil {
		return &NotFoundError{Resource: "task", ID: id}
	}

	// Unschedule before deleting.
	if s.scheduler != nil {
		s.scheduler.UnscheduleTask(id)
	}

	if err := s.repo.DeleteTask(ctx, id); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf("deleting task %q: %w", id, err)
	}
	s.logger.Info("task deleted", "id", id)
	return nil
}

func (s *taskService) PauseTask(ctx context.Context, id string) (*storage.ScheduledTask, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.pause")
	defer span.End()

	task, err := s.repo.GetTask(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf(errFmtLookingUpTask, err)
	}
	if task == nil {
		return nil, &NotFoundError{Resource: "task", ID: id}
	}

	task.Status = storage.TaskStatusPaused
	if err := s.repo.UpdateTask(ctx, task); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("pausing task: %w", err)
	}

	s.logger.Info("task paused", "id", id)

	// Remove from scheduler.
	if s.scheduler != nil {
		s.scheduler.UnscheduleTask(id)
	}

	return task, nil
}

func (s *taskService) ResumeTask(ctx context.Context, id string) (*storage.ScheduledTask, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.resume")
	defer span.End()

	task, err := s.repo.GetTask(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf(errFmtLookingUpTask, err)
	}
	if task == nil {
		return nil, &NotFoundError{Resource: "task", ID: id}
	}

	task.Status = storage.TaskStatusActive
	task.RunCount = 0
	task.LastRunAt = nil
	task.LastRunStatus = ""
	if err := s.repo.UpdateTask(ctx, task); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("resuming task: %w", err)
	}

	s.logger.Info("task resumed", "id", id)

	// Re-add to scheduler.
	if s.scheduler != nil {
		if schedErr := s.scheduler.ScheduleTask(task); schedErr != nil {
			s.logger.Warn("failed to schedule resumed task", "task_id", id, "error", schedErr)
		}
	}

	return task, nil
}

func (s *taskService) ListJobHistory(ctx context.Context, taskID string, limit int) ([]*storage.JobHistory, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.list_job_history")
	defer span.End()

	if limit <= 0 {
		limit = 50
	}
	history, err := s.repo.ListJobHistory(ctx, taskID, limit)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("listing job history: %w", err)
	}
	return history, nil
}

func (s *taskService) ListAllJobHistory(ctx context.Context, limit, offset int) ([]*storage.JobHistory, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.list_all_job_history")
	defer span.End()

	if limit <= 0 {
		limit = 50
	}
	if offset < 0 {
		offset = 0
	}
	history, err := s.repo.ListAllJobHistory(ctx, limit, offset)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("listing all job history: %w", err)
	}
	return history, nil
}

func (s *taskService) GetJobHistory(ctx context.Context, id string) (*storage.JobHistory, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.get_job_history")
	defer span.End()

	jh, err := s.repo.GetJobHistory(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("getting job history %q: %w", id, err)
	}
	if jh == nil {
		return nil, &NotFoundError{Resource: "job_history", ID: id}
	}
	return jh, nil
}

func (s *taskService) DeleteJobHistory(ctx context.Context, id string) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.delete_job_history")
	defer span.End()

	jh, err := s.repo.GetJobHistory(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf("looking up job history: %w", err)
	}
	if jh == nil {
		return &NotFoundError{Resource: "job_history", ID: id}
	}
	if err := s.repo.DeleteJobHistory(ctx, id); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf("deleting job history %q: %w", id, err)
	}
	s.logger.Info("job history deleted", "id", id)
	return nil
}

func (s *taskService) BulkDeleteJobHistory(ctx context.Context, ids []string) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "task.bulk_delete_job_history")
	defer span.End()

	if err := s.repo.BulkDeleteJobHistory(ctx, ids); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf("bulk deleting job history: %w", err)
	}
	s.logger.Info("job history bulk deleted", "count", len(ids))
	return nil
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
	case storage.ScheduleRunImmediately, storage.ScheduleOneOff, storage.ScheduleInterval, storage.ScheduleCron:
		// valid
	case "":
		task.ScheduleType = storage.ScheduleRunImmediately
	default:
		return &ValidationError{Field: "schedule_type", Message: "must be run_immediately, one_off, interval, or cron"}
	}

	return validateScheduleConfig(task)
}

func validateScheduleConfig(task *storage.ScheduledTask) error {
	cfg := task.ScheduleConfig
	switch task.ScheduleType {
	case storage.ScheduleRunImmediately:
		// No schedule config needed — task runs immediately on creation.
		return nil
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
