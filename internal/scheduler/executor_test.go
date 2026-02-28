package scheduler_test

import (
	"context"
	"errors"
	"log/slog"
	"os"
	"sync"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/scheduler"
	"github.com/shaharia-lab/agento/internal/storage"
)

// --- helpers ---

func newTestLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelError}))
}

// --- TaskStore stub ---

type stubTaskStore struct {
	mu      sync.Mutex
	tasks   map[string]*storage.ScheduledTask
	history []*storage.JobHistory
}

func newStubTaskStore(tasks ...*storage.ScheduledTask) *stubTaskStore {
	s := &stubTaskStore{tasks: make(map[string]*storage.ScheduledTask)}
	for _, t := range tasks {
		s.tasks[t.ID] = t
	}
	return s
}

func (s *stubTaskStore) GetTask(id string) (*storage.ScheduledTask, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.tasks[id], nil
}

func (s *stubTaskStore) ListTasks() ([]*storage.ScheduledTask, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]*storage.ScheduledTask, 0, len(s.tasks))
	for _, t := range s.tasks {
		out = append(out, t)
	}
	return out, nil
}

func (s *stubTaskStore) CreateTask(_ *storage.ScheduledTask) error { return nil }

func (s *stubTaskStore) UpdateTask(task *storage.ScheduledTask) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.tasks[task.ID] = task
	return nil
}

func (s *stubTaskStore) DeleteTask(_ string) error { return nil }

func (s *stubTaskStore) CreateJobHistory(jh *storage.JobHistory) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.history = append(s.history, jh)
	return nil
}

func (s *stubTaskStore) UpdateJobHistory(jh *storage.JobHistory) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i, h := range s.history {
		if h.TaskID == jh.TaskID {
			s.history[i] = jh
			return nil
		}
	}
	s.history = append(s.history, jh)
	return nil
}

func (s *stubTaskStore) GetJobHistory(_ string) (*storage.JobHistory, error) { return nil, nil }
func (s *stubTaskStore) ListJobHistory(_ string, _ int) ([]*storage.JobHistory, error) {
	return nil, nil
}
func (s *stubTaskStore) ListAllJobHistory(_, _ int) ([]*storage.JobHistory, error) {
	return nil, nil
}

// --- ChatStore stub ---

type stubChatStore struct {
	createErr error
}

func (c *stubChatStore) ListSessions() ([]*storage.ChatSession, error)     { return nil, nil }
func (c *stubChatStore) GetSession(_ string) (*storage.ChatSession, error) { return nil, nil }
func (c *stubChatStore) GetSessionWithMessages(_ string) (*storage.ChatSession, []storage.ChatMessage, error) {
	return nil, nil, nil
}
func (c *stubChatStore) CreateSession(_, _, _, _ string) (*storage.ChatSession, error) {
	if c.createErr != nil {
		return nil, c.createErr
	}
	return &storage.ChatSession{ID: "session-123"}, nil
}
func (c *stubChatStore) AppendMessage(_ string, _ storage.ChatMessage) error { return nil }
func (c *stubChatStore) UpdateSession(_ *storage.ChatSession) error          { return nil }
func (c *stubChatStore) DeleteSession(_ string) error                        { return nil }

// --- EventPublisher stub ---

type stubEventPublisher struct {
	mu     sync.Mutex
	events []publishedEvent
}

type publishedEvent struct {
	eventType string
	payload   map[string]string
}

func (p *stubEventPublisher) Publish(eventType string, payload map[string]string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.events = append(p.events, publishedEvent{eventType: eventType, payload: payload})
}

func (p *stubEventPublisher) waitForEvents(n int, timeout time.Duration) []publishedEvent {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		p.mu.Lock()
		got := len(p.events)
		p.mu.Unlock()
		if got >= n {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	out := make([]publishedEvent, len(p.events))
	copy(out, p.events)
	return out
}

// --- task builder ---

func buildTask(id, name string) *storage.ScheduledTask {
	return &storage.ScheduledTask{
		ID:             id,
		Name:           name,
		Description:    "A test task",
		Prompt:         "hello world",
		AgentSlug:      "my-agent",
		Status:         storage.TaskStatusActive,
		ScheduleType:   storage.ScheduleInterval,
		TimeoutMinutes: 1,
		ScheduleConfig: storage.ScheduleConfig{EveryMinutes: 5},
	}
}

// --- tests ---

// TestPublishTaskFailed_NoEventPublisher verifies that a nil EventPublisher
// does not panic when a task fails.
func TestPublishTaskFailed_NoEventPublisher(t *testing.T) {
	task := buildTask("t1", "Task 1")
	ts := newStubTaskStore(task)
	chatStore := &stubChatStore{createErr: errors.New("db unavailable")}

	s, err := scheduler.New(scheduler.Config{
		TaskStore: ts,
		ChatStore: chatStore,
		Logger:    newTestLogger(),
		// EventPublisher intentionally nil
	})
	require.NoError(t, err)

	// Should not panic even though EventPublisher is nil.
	assert.NotPanics(t, func() {
		s.ExportedExecuteTask(task.ID)
	})
}

// TestPublishTaskFailed_WhenSessionCreationFails verifies EventTaskFailed is
// published when the chat session cannot be created.
func TestPublishTaskFailed_WhenSessionCreationFails(t *testing.T) {
	task := buildTask("t2", "Session Fail Task")
	ts := newStubTaskStore(task)
	chatStore := &stubChatStore{createErr: errors.New("db error")}
	pub := &stubEventPublisher{}

	s, err := scheduler.New(scheduler.Config{
		TaskStore:      ts,
		ChatStore:      chatStore,
		Logger:         newTestLogger(),
		MaxConcurrency: 1,
		EventPublisher: pub,
	})
	require.NoError(t, err)

	s.ExportedExecuteTask(task.ID)

	events := pub.waitForEvents(1, 500*time.Millisecond)
	require.Len(t, events, 1)
	assert.Equal(t, scheduler.EventTaskFailed, events[0].eventType)
	assert.Equal(t, "t2", events[0].payload["Task ID"])
	assert.Equal(t, "Session Fail Task", events[0].payload["Task Name"])
	assert.Equal(t, "A test task", events[0].payload["Task Description"])
	assert.Equal(t, "my-agent", events[0].payload["Agent"])
	assert.Equal(t, "Failed", events[0].payload["Status"])
	assert.NotEmpty(t, events[0].payload["Error"])
}

// TestPublishTaskFailed_PayloadHasAllRequiredFields checks that the failed
// event payload includes all expected keys.
func TestPublishTaskFailed_PayloadHasAllRequiredFields(t *testing.T) {
	task := buildTask("t3", "Payload Test Task")
	ts := newStubTaskStore(task)
	chatStore := &stubChatStore{createErr: errors.New("forced failure")}
	pub := &stubEventPublisher{}

	s, err := scheduler.New(scheduler.Config{
		TaskStore:      ts,
		ChatStore:      chatStore,
		Logger:         newTestLogger(),
		MaxConcurrency: 1,
		EventPublisher: pub,
	})
	require.NoError(t, err)

	s.ExportedExecuteTask(task.ID)
	events := pub.waitForEvents(1, 500*time.Millisecond)
	require.Len(t, events, 1)

	payload := events[0].payload
	requiredKeys := []string{"Task ID", "Task Name", "Task Description", "Agent", "Status", "Error", "Run Count"}
	for _, key := range requiredKeys {
		assert.Contains(t, payload, key, "payload should contain key %q", key)
	}
}

// TestEventConstants verifies the public event type constants.
func TestEventConstants(t *testing.T) {
	assert.Equal(t, "tasks_scheduler.task_execution.finished", scheduler.EventTaskFinished)
	assert.Equal(t, "tasks_scheduler.task_execution.failed", scheduler.EventTaskFailed)
}

// TestScheduler_PausedTaskNotExecuted verifies that paused tasks are skipped.
func TestScheduler_PausedTaskNotExecuted(t *testing.T) {
	paused := buildTask("paused-01", "Paused Task")
	paused.Status = storage.TaskStatusPaused
	ts := newStubTaskStore(paused)
	pub := &stubEventPublisher{}

	s, err := scheduler.New(scheduler.Config{
		TaskStore:      ts,
		Logger:         newTestLogger(),
		MaxConcurrency: 1,
		EventPublisher: pub,
	})
	require.NoError(t, err)
	require.NoError(t, s.Start(context.Background()))
	defer s.Stop() //nolint:errcheck

	// Give the scheduler a brief window; no events should fire.
	time.Sleep(60 * time.Millisecond)
	events := pub.waitForEvents(1, 100*time.Millisecond)
	assert.Empty(t, events, "paused task must not trigger events")
}

// TestScheduler_NoPublisherOnPausedTask verifies that executeTask with nil
// publisher skips gracefully even for a paused task.
func TestScheduler_NoPublisherOnPausedTask(t *testing.T) {
	task := buildTask("t4", "Active Task")
	task.Status = storage.TaskStatusPaused // will exit early
	ts := newStubTaskStore(task)

	s, err := scheduler.New(scheduler.Config{
		TaskStore:      ts,
		Logger:         newTestLogger(),
		MaxConcurrency: 1,
		// No EventPublisher
	})
	require.NoError(t, err)

	assert.NotPanics(t, func() {
		s.ExportedExecuteTask(task.ID)
	})
}
