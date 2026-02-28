package mocks

import (
	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/storage"
)

// MockTaskStore is a mock implementation of storage.TaskStore.
type MockTaskStore struct {
	mock.Mock
}

//nolint:revive
func (m *MockTaskStore) ListTasks() ([]*storage.ScheduledTask, error) {
	args := m.Called()
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*storage.ScheduledTask), args.Error(1)
}

//nolint:revive
func (m *MockTaskStore) GetTask(id string) (*storage.ScheduledTask, error) {
	args := m.Called(id)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.ScheduledTask), args.Error(1)
}

//nolint:revive
func (m *MockTaskStore) CreateTask(task *storage.ScheduledTask) error {
	args := m.Called(task)
	return args.Error(0)
}

//nolint:revive
func (m *MockTaskStore) UpdateTask(task *storage.ScheduledTask) error {
	args := m.Called(task)
	return args.Error(0)
}

//nolint:revive
func (m *MockTaskStore) DeleteTask(id string) error {
	args := m.Called(id)
	return args.Error(0)
}

//nolint:revive
func (m *MockTaskStore) ListJobHistory(taskID string, limit int) ([]*storage.JobHistory, error) {
	args := m.Called(taskID, limit)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*storage.JobHistory), args.Error(1)
}

//nolint:revive
func (m *MockTaskStore) ListAllJobHistory(limit, offset int) ([]*storage.JobHistory, error) {
	args := m.Called(limit, offset)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*storage.JobHistory), args.Error(1)
}

//nolint:revive
func (m *MockTaskStore) GetJobHistory(id string) (*storage.JobHistory, error) {
	args := m.Called(id)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.JobHistory), args.Error(1)
}

//nolint:revive
func (m *MockTaskStore) CreateJobHistory(jh *storage.JobHistory) error {
	args := m.Called(jh)
	return args.Error(0)
}

//nolint:revive
func (m *MockTaskStore) UpdateJobHistory(jh *storage.JobHistory) error {
	args := m.Called(jh)
	return args.Error(0)
}
