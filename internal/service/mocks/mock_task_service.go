package mocks

import (
	"context"

	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/storage"
)

// MockTaskService is a mock implementation of service.TaskService.
type MockTaskService struct {
	mock.Mock
}

//nolint:revive
func (m *MockTaskService) ListTasks(ctx context.Context) ([]*storage.ScheduledTask, error) {
	args := m.Called(ctx)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*storage.ScheduledTask), args.Error(1)
}

//nolint:revive
func (m *MockTaskService) GetTask(ctx context.Context, id string) (*storage.ScheduledTask, error) {
	args := m.Called(ctx, id)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.ScheduledTask), args.Error(1)
}

//nolint:revive
func (m *MockTaskService) CreateTask(ctx context.Context, task *storage.ScheduledTask) (*storage.ScheduledTask, error) {
	args := m.Called(ctx, task)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.ScheduledTask), args.Error(1)
}

//nolint:revive
func (m *MockTaskService) UpdateTask(ctx context.Context, id string, task *storage.ScheduledTask) (*storage.ScheduledTask, error) {
	args := m.Called(ctx, id, task)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.ScheduledTask), args.Error(1)
}

//nolint:revive
func (m *MockTaskService) DeleteTask(ctx context.Context, id string) error {
	args := m.Called(ctx, id)
	return args.Error(0)
}

//nolint:revive
func (m *MockTaskService) PauseTask(ctx context.Context, id string) (*storage.ScheduledTask, error) {
	args := m.Called(ctx, id)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.ScheduledTask), args.Error(1)
}

//nolint:revive
func (m *MockTaskService) ResumeTask(ctx context.Context, id string) (*storage.ScheduledTask, error) {
	args := m.Called(ctx, id)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.ScheduledTask), args.Error(1)
}

//nolint:revive
func (m *MockTaskService) ListJobHistory(ctx context.Context, taskID string, limit int) ([]*storage.JobHistory, error) {
	args := m.Called(ctx, taskID, limit)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*storage.JobHistory), args.Error(1)
}

//nolint:revive
func (m *MockTaskService) ListAllJobHistory(ctx context.Context, limit, offset int) ([]*storage.JobHistory, error) {
	args := m.Called(ctx, limit, offset)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*storage.JobHistory), args.Error(1)
}

//nolint:revive
func (m *MockTaskService) GetJobHistory(ctx context.Context, id string) (*storage.JobHistory, error) {
	args := m.Called(ctx, id)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.JobHistory), args.Error(1)
}
