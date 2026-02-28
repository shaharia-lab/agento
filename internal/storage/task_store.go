package storage

import (
	"encoding/json"
	"time"
)

// ScheduleType defines the kind of schedule for a task.
type ScheduleType string

// Schedule type constants for task scheduling.
const (
	ScheduleOneOff   ScheduleType = "one_off"
	ScheduleInterval ScheduleType = "interval"
	ScheduleCron     ScheduleType = "cron"
)

// TaskStatus defines the lifecycle state of a scheduled task.
type TaskStatus string

// Task status constants for lifecycle management.
const (
	TaskStatusActive TaskStatus = "active"
	TaskStatusPaused TaskStatus = "paused"
)

// JobStatus defines the outcome of a single job execution.
type JobStatus string

// Job status constants for execution outcomes.
const (
	JobStatusRunning JobStatus = "running"
	JobStatusSuccess JobStatus = "success"
	JobStatusFailed  JobStatus = "failed"
)

// ScheduleConfig holds the schedule-type-specific configuration as JSON.
type ScheduleConfig struct {
	// One-off
	RunAt string `json:"run_at,omitempty"`
	// Interval
	EveryMinutes int    `json:"every_minutes,omitempty"`
	EveryHours   int    `json:"every_hours,omitempty"`
	EveryDays    int    `json:"every_days,omitempty"`
	AtTime       string `json:"at_time,omitempty"` // HH:MM for daily intervals
	// Cron
	Expression string `json:"expression,omitempty"`
}

// ScheduledTask represents a task that can be run on a schedule.
type ScheduledTask struct {
	ID                string         `json:"id"`
	Name              string         `json:"name"`
	Description       string         `json:"description"`
	Prompt            string         `json:"prompt"`
	AgentSlug         string         `json:"agent_slug"`
	WorkingDirectory  string         `json:"working_directory"`
	Model             string         `json:"model"`
	SettingsProfileID string         `json:"settings_profile_id"`
	TimeoutMinutes    int            `json:"timeout_minutes"`
	ScheduleType      ScheduleType   `json:"schedule_type"`
	ScheduleConfig    ScheduleConfig `json:"schedule_config"`
	StopAfterCount    int            `json:"stop_after_count"`
	StopAfterTime     *time.Time     `json:"stop_after_time,omitempty"`
	Status            TaskStatus     `json:"status"`
	RunCount          int            `json:"run_count"`
	LastRunAt         *time.Time     `json:"last_run_at,omitempty"`
	LastRunStatus     string         `json:"last_run_status"`
	NextRunAt         *time.Time     `json:"next_run_at,omitempty"`
	CreatedAt         time.Time      `json:"created_at"`
	UpdatedAt         time.Time      `json:"updated_at"`
}

// MarshalScheduleConfig returns the JSON encoding of the schedule config.
func (t *ScheduledTask) MarshalScheduleConfig() (string, error) {
	b, err := json.Marshal(t.ScheduleConfig)
	if err != nil {
		return "", err
	}
	return string(b), nil
}

// JobHistory records the result of a single task execution.
type JobHistory struct {
	ID                       string     `json:"id"`
	TaskID                   string     `json:"task_id"`
	TaskName                 string     `json:"task_name"`
	AgentSlug                string     `json:"agent_slug"`
	Status                   JobStatus  `json:"status"`
	StartedAt                time.Time  `json:"started_at"`
	FinishedAt               *time.Time `json:"finished_at,omitempty"`
	DurationMS               int64      `json:"duration_ms"`
	ChatSessionID            string     `json:"chat_session_id"`
	Model                    string     `json:"model"`
	PromptPreview            string     `json:"prompt_preview"`
	ErrorMessage             string     `json:"error_message"`
	TotalInputTokens         int        `json:"total_input_tokens"`
	TotalOutputTokens        int        `json:"total_output_tokens"`
	TotalCacheCreationTokens int        `json:"total_cache_creation_tokens"`
	TotalCacheReadTokens     int        `json:"total_cache_read_tokens"`
}

// TaskStore defines the persistence interface for scheduled tasks and job history.
type TaskStore interface {
	ListTasks() ([]*ScheduledTask, error)
	GetTask(id string) (*ScheduledTask, error)
	CreateTask(task *ScheduledTask) error
	UpdateTask(task *ScheduledTask) error
	DeleteTask(id string) error

	ListJobHistory(taskID string, limit int) ([]*JobHistory, error)
	ListAllJobHistory(limit, offset int) ([]*JobHistory, error)
	GetJobHistory(id string) (*JobHistory, error)
	CreateJobHistory(jh *JobHistory) error
	UpdateJobHistory(jh *JobHistory) error
}
