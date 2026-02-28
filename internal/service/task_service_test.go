package service

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/storage"
	"github.com/shaharia-lab/agento/internal/storage/mocks"
)

func newTestTaskService(repo *mocks.MockTaskStore) TaskService {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	return NewTaskService(repo, logger)
}

// ---------------------------------------------------------------------------
// ListTasks
// ---------------------------------------------------------------------------

func TestListTasks(t *testing.T) {
	tasks := []*storage.ScheduledTask{
		{ID: "t1", Name: "Task 1"},
		{ID: "t2", Name: "Task 2"},
	}

	repo := new(mocks.MockTaskStore)
	repo.On("ListTasks").Return(tasks, nil)

	svc := newTestTaskService(repo)
	result, err := svc.ListTasks(context.Background())

	require.NoError(t, err)
	assert.Len(t, result, 2)
	repo.AssertExpectations(t)
}

func TestListTasks_Error(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("ListTasks").Return(nil, errors.New("db error"))

	svc := newTestTaskService(repo)
	_, err := svc.ListTasks(context.Background())

	require.Error(t, err)
	assert.Contains(t, err.Error(), "listing tasks")
	repo.AssertExpectations(t)
}

// ---------------------------------------------------------------------------
// GetTask
// ---------------------------------------------------------------------------

func TestGetTask(t *testing.T) {
	task := &storage.ScheduledTask{ID: "t1", Name: "Task 1"}

	repo := new(mocks.MockTaskStore)
	repo.On("GetTask", "t1").Return(task, nil)

	svc := newTestTaskService(repo)
	result, err := svc.GetTask(context.Background(), "t1")

	require.NoError(t, err)
	assert.Equal(t, "Task 1", result.Name)
	repo.AssertExpectations(t)
}

func TestGetTask_NotFound(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("GetTask", "missing").Return(nil, nil)

	svc := newTestTaskService(repo)
	_, err := svc.GetTask(context.Background(), "missing")

	require.Error(t, err)
	var notFound *NotFoundError
	assert.True(t, errors.As(err, &notFound))
	repo.AssertExpectations(t)
}

// ---------------------------------------------------------------------------
// CreateTask
// ---------------------------------------------------------------------------

func TestCreateTask(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("CreateTask", mock.AnythingOfType("*storage.ScheduledTask")).Return(nil)

	svc := newTestTaskService(repo)
	task := &storage.ScheduledTask{
		Name:         "New Task",
		Prompt:       "Do something",
		ScheduleType: storage.ScheduleOneOff,
		ScheduleConfig: storage.ScheduleConfig{
			RunAt: "2026-03-01T10:00:00Z",
		},
	}
	result, err := svc.CreateTask(context.Background(), task)

	require.NoError(t, err)
	assert.Equal(t, storage.TaskStatusActive, result.Status)
	assert.Equal(t, 30, result.TimeoutMinutes)
	repo.AssertExpectations(t)
}

func TestCreateTask_ValidationErrors(t *testing.T) {
	tests := []struct {
		name    string
		task    *storage.ScheduledTask
		wantErr string
	}{
		{
			name:    "missing name",
			task:    &storage.ScheduledTask{Prompt: "p", ScheduleType: storage.ScheduleOneOff, ScheduleConfig: storage.ScheduleConfig{RunAt: "t"}},
			wantErr: "name",
		},
		{
			name:    "missing prompt",
			task:    &storage.ScheduledTask{Name: "n", ScheduleType: storage.ScheduleOneOff, ScheduleConfig: storage.ScheduleConfig{RunAt: "t"}},
			wantErr: "prompt",
		},
		{
			name:    "invalid schedule type",
			task:    &storage.ScheduledTask{Name: "n", Prompt: "p", ScheduleType: "bogus"},
			wantErr: "schedule_type",
		},
		{
			name:    "one_off missing run_at",
			task:    &storage.ScheduledTask{Name: "n", Prompt: "p", ScheduleType: storage.ScheduleOneOff},
			wantErr: "run_at",
		},
		{
			name:    "interval missing duration",
			task:    &storage.ScheduledTask{Name: "n", Prompt: "p", ScheduleType: storage.ScheduleInterval},
			wantErr: "schedule_config",
		},
		{
			name:    "cron missing expression",
			task:    &storage.ScheduledTask{Name: "n", Prompt: "p", ScheduleType: storage.ScheduleCron},
			wantErr: "expression",
		},
		{
			name:    "timeout too high",
			task:    &storage.ScheduledTask{Name: "n", Prompt: "p", ScheduleType: storage.ScheduleOneOff, ScheduleConfig: storage.ScheduleConfig{RunAt: "t"}, TimeoutMinutes: 300},
			wantErr: "timeout",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			repo := new(mocks.MockTaskStore)
			svc := newTestTaskService(repo)

			_, err := svc.CreateTask(context.Background(), tt.task)

			require.Error(t, err)
			var ve *ValidationError
			assert.True(t, errors.As(err, &ve), "expected ValidationError")
			assert.Contains(t, ve.Field, tt.wantErr)
			repo.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// UpdateTask
// ---------------------------------------------------------------------------

func TestUpdateTask(t *testing.T) {
	now := time.Now().UTC()
	existing := &storage.ScheduledTask{
		ID:            "t1",
		Name:          "Old Name",
		Prompt:        "old prompt",
		RunCount:      5,
		LastRunAt:     &now,
		LastRunStatus: "success",
		CreatedAt:     now,
		ScheduleType:  storage.ScheduleOneOff,
		ScheduleConfig: storage.ScheduleConfig{
			RunAt: "2026-01-01T00:00:00Z",
		},
	}

	repo := new(mocks.MockTaskStore)
	repo.On("GetTask", "t1").Return(existing, nil)
	repo.On("UpdateTask", mock.MatchedBy(func(t *storage.ScheduledTask) bool {
		// Preserves run metadata from existing task
		return t.RunCount == 5 && t.LastRunStatus == "success" && t.CreatedAt.Equal(now)
	})).Return(nil)

	svc := newTestTaskService(repo)
	updated := &storage.ScheduledTask{
		Name:         "New Name",
		Prompt:       "new prompt",
		ScheduleType: storage.ScheduleOneOff,
		ScheduleConfig: storage.ScheduleConfig{
			RunAt: "2026-06-01T00:00:00Z",
		},
	}
	result, err := svc.UpdateTask(context.Background(), "t1", updated)

	require.NoError(t, err)
	assert.Equal(t, "t1", result.ID)
	assert.Equal(t, 5, result.RunCount)
	repo.AssertExpectations(t)
}

func TestUpdateTask_NotFound(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("GetTask", "missing").Return(nil, nil)

	svc := newTestTaskService(repo)
	_, err := svc.UpdateTask(context.Background(), "missing", &storage.ScheduledTask{Name: "x", Prompt: "p"})

	require.Error(t, err)
	var notFound *NotFoundError
	assert.True(t, errors.As(err, &notFound))
	repo.AssertExpectations(t)
}

// ---------------------------------------------------------------------------
// DeleteTask
// ---------------------------------------------------------------------------

func TestDeleteTask(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("GetTask", "t1").Return(&storage.ScheduledTask{ID: "t1"}, nil)
	repo.On("DeleteTask", "t1").Return(nil)

	svc := newTestTaskService(repo)
	err := svc.DeleteTask(context.Background(), "t1")

	require.NoError(t, err)
	repo.AssertExpectations(t)
}

func TestDeleteTask_NotFound(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("GetTask", "missing").Return(nil, nil)

	svc := newTestTaskService(repo)
	err := svc.DeleteTask(context.Background(), "missing")

	require.Error(t, err)
	var notFound *NotFoundError
	assert.True(t, errors.As(err, &notFound))
	repo.AssertExpectations(t)
}

// ---------------------------------------------------------------------------
// PauseTask
// ---------------------------------------------------------------------------

func TestPauseTask(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("GetTask", "t1").Return(&storage.ScheduledTask{ID: "t1", Status: storage.TaskStatusActive}, nil)
	repo.On("UpdateTask", mock.MatchedBy(func(task *storage.ScheduledTask) bool {
		return task.Status == storage.TaskStatusPaused
	})).Return(nil)

	svc := newTestTaskService(repo)
	result, err := svc.PauseTask(context.Background(), "t1")

	require.NoError(t, err)
	assert.Equal(t, storage.TaskStatusPaused, result.Status)
	repo.AssertExpectations(t)
}

func TestPauseTask_NotFound(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("GetTask", "missing").Return(nil, nil)

	svc := newTestTaskService(repo)
	_, err := svc.PauseTask(context.Background(), "missing")

	require.Error(t, err)
	var notFound *NotFoundError
	assert.True(t, errors.As(err, &notFound))
}

// ---------------------------------------------------------------------------
// ResumeTask
// ---------------------------------------------------------------------------

func TestResumeTask(t *testing.T) {
	now := time.Now().UTC()
	repo := new(mocks.MockTaskStore)
	repo.On("GetTask", "t1").Return(&storage.ScheduledTask{
		ID:            "t1",
		Status:        storage.TaskStatusPaused,
		RunCount:      3,
		LastRunAt:     &now,
		LastRunStatus: "failed",
	}, nil)
	repo.On("UpdateTask", mock.MatchedBy(func(task *storage.ScheduledTask) bool {
		return task.Status == storage.TaskStatusActive &&
			task.RunCount == 0 &&
			task.LastRunAt == nil &&
			task.LastRunStatus == ""
	})).Return(nil)

	svc := newTestTaskService(repo)
	result, err := svc.ResumeTask(context.Background(), "t1")

	require.NoError(t, err)
	assert.Equal(t, storage.TaskStatusActive, result.Status)
	assert.Equal(t, 0, result.RunCount, "run count should be reset on resume")
	assert.Nil(t, result.LastRunAt, "last_run_at should be cleared on resume")
	assert.Empty(t, result.LastRunStatus, "last_run_status should be cleared on resume")
	repo.AssertExpectations(t)
}

func TestResumeTask_NotFound(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("GetTask", "missing").Return(nil, nil)

	svc := newTestTaskService(repo)
	_, err := svc.ResumeTask(context.Background(), "missing")

	require.Error(t, err)
	var notFound *NotFoundError
	assert.True(t, errors.As(err, &notFound))
}

func TestResumeTask_StoreError(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("GetTask", "t1").Return(&storage.ScheduledTask{
		ID:     "t1",
		Status: storage.TaskStatusPaused,
	}, nil)
	repo.On("UpdateTask", mock.Anything).Return(errors.New("db write error"))

	svc := newTestTaskService(repo)
	_, err := svc.ResumeTask(context.Background(), "t1")

	require.Error(t, err)
	assert.Contains(t, err.Error(), "resuming task")
	repo.AssertExpectations(t)
}

// ---------------------------------------------------------------------------
// ListJobHistory
// ---------------------------------------------------------------------------

func TestListJobHistory(t *testing.T) {
	history := []*storage.JobHistory{
		{ID: "j1", TaskID: "t1"},
	}

	repo := new(mocks.MockTaskStore)
	repo.On("ListJobHistory", "t1", 50).Return(history, nil)

	svc := newTestTaskService(repo)
	result, err := svc.ListJobHistory(context.Background(), "t1", 0)

	require.NoError(t, err)
	assert.Len(t, result, 1)
	repo.AssertExpectations(t)
}

func TestListAllJobHistory(t *testing.T) {
	history := []*storage.JobHistory{{ID: "j1"}, {ID: "j2"}}

	repo := new(mocks.MockTaskStore)
	repo.On("ListAllJobHistory", 50, 0).Return(history, nil)

	svc := newTestTaskService(repo)
	result, err := svc.ListAllJobHistory(context.Background(), 0, -1)

	require.NoError(t, err)
	assert.Len(t, result, 2)
	repo.AssertExpectations(t)
}

// ---------------------------------------------------------------------------
// GetJobHistory
// ---------------------------------------------------------------------------

func TestGetJobHistory(t *testing.T) {
	jh := &storage.JobHistory{ID: "j1", TaskName: "Test"}

	repo := new(mocks.MockTaskStore)
	repo.On("GetJobHistory", "j1").Return(jh, nil)

	svc := newTestTaskService(repo)
	result, err := svc.GetJobHistory(context.Background(), "j1")

	require.NoError(t, err)
	assert.Equal(t, "Test", result.TaskName)
	repo.AssertExpectations(t)
}

func TestGetJobHistory_NotFound(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("GetJobHistory", "missing").Return(nil, nil)

	svc := newTestTaskService(repo)
	_, err := svc.GetJobHistory(context.Background(), "missing")

	require.Error(t, err)
	var notFound *NotFoundError
	assert.True(t, errors.As(err, &notFound))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

func TestValidateTask_EmptyScheduleTypeDefaultsToOneOff(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("CreateTask", mock.MatchedBy(func(task *storage.ScheduledTask) bool {
		return task.ScheduleType == storage.ScheduleOneOff
	})).Return(nil)

	svc := newTestTaskService(repo)
	task := &storage.ScheduledTask{
		Name:           "Test",
		Prompt:         "Do something",
		ScheduleType:   "", // empty
		ScheduleConfig: storage.ScheduleConfig{RunAt: "2026-03-01T10:00:00Z"},
	}
	_, err := svc.CreateTask(context.Background(), task)

	require.NoError(t, err)
	assert.Equal(t, storage.ScheduleOneOff, task.ScheduleType)
	repo.AssertExpectations(t)
}

func TestCreateTask_ValidIntervalSchedule(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("CreateTask", mock.Anything).Return(nil)

	svc := newTestTaskService(repo)
	task := &storage.ScheduledTask{
		Name:           "Interval Task",
		Prompt:         "Run periodically",
		ScheduleType:   storage.ScheduleInterval,
		ScheduleConfig: storage.ScheduleConfig{EveryMinutes: 30},
	}
	_, err := svc.CreateTask(context.Background(), task)

	require.NoError(t, err)
	repo.AssertExpectations(t)
}

func TestCreateTask_ValidCronSchedule(t *testing.T) {
	repo := new(mocks.MockTaskStore)
	repo.On("CreateTask", mock.Anything).Return(nil)

	svc := newTestTaskService(repo)
	task := &storage.ScheduledTask{
		Name:           "Cron Task",
		Prompt:         "Run on schedule",
		ScheduleType:   storage.ScheduleCron,
		ScheduleConfig: storage.ScheduleConfig{Expression: "0 */2 * * *"},
	}
	_, err := svc.CreateTask(context.Background(), task)

	require.NoError(t, err)
	repo.AssertExpectations(t)
}
