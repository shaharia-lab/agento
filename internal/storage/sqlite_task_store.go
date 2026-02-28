package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"
)

// SQLiteTaskStore implements TaskStore backed by a SQLite database.
type SQLiteTaskStore struct {
	db *sql.DB
}

// NewSQLiteTaskStore returns a new SQLiteTaskStore.
func NewSQLiteTaskStore(db *sql.DB) *SQLiteTaskStore {
	return &SQLiteTaskStore{db: db}
}

// ListTasks returns all scheduled tasks ordered by creation time descending.
func (s *SQLiteTaskStore) ListTasks() ([]*ScheduledTask, error) {
	ctx := context.Background()
	rows, err := s.db.QueryContext(ctx, `
		SELECT id, name, description, prompt, agent_slug, working_directory, model,
		       settings_profile_id, timeout_minutes, schedule_type, schedule_config,
		       stop_after_count, stop_after_time, status, run_count, last_run_at,
		       last_run_status, next_run_at, created_at, updated_at
		FROM scheduled_tasks
		ORDER BY created_at DESC`)
	if err != nil {
		return nil, fmt.Errorf("listing tasks: %w", err)
	}
	defer rows.Close() //nolint:errcheck

	tasks := make([]*ScheduledTask, 0)
	for rows.Next() {
		t, err := scanScheduledTask(rows)
		if err != nil {
			return nil, err
		}
		tasks = append(tasks, t)
	}
	return tasks, rows.Err()
}

// GetTask returns a scheduled task by ID, or nil if not found.
func (s *SQLiteTaskStore) GetTask(id string) (*ScheduledTask, error) {
	ctx := context.Background()
	row := s.db.QueryRowContext(ctx, `
		SELECT id, name, description, prompt, agent_slug, working_directory, model,
		       settings_profile_id, timeout_minutes, schedule_type, schedule_config,
		       stop_after_count, stop_after_time, status, run_count, last_run_at,
		       last_run_status, next_run_at, created_at, updated_at
		FROM scheduled_tasks WHERE id = ?`, id)

	t := &ScheduledTask{}
	var configJSON string
	var stopAfterTime sql.NullTime
	var lastRunAt sql.NullTime
	var nextRunAt sql.NullTime

	err := row.Scan(
		&t.ID, &t.Name, &t.Description, &t.Prompt, &t.AgentSlug,
		&t.WorkingDirectory, &t.Model, &t.SettingsProfileID, &t.TimeoutMinutes,
		&t.ScheduleType, &configJSON, &t.StopAfterCount, &stopAfterTime,
		&t.Status, &t.RunCount, &lastRunAt, &t.LastRunStatus, &nextRunAt,
		&t.CreatedAt, &t.UpdatedAt,
	)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("getting task %q: %w", id, err)
	}

	if err := json.Unmarshal([]byte(configJSON), &t.ScheduleConfig); err != nil {
		return nil, fmt.Errorf("unmarshaling schedule config for task %q: %w", id, err)
	}
	if stopAfterTime.Valid {
		t.StopAfterTime = &stopAfterTime.Time
	}
	if lastRunAt.Valid {
		t.LastRunAt = &lastRunAt.Time
	}
	if nextRunAt.Valid {
		t.NextRunAt = &nextRunAt.Time
	}
	return t, nil
}

// CreateTask inserts a new scheduled task.
func (s *SQLiteTaskStore) CreateTask(task *ScheduledTask) error {
	if task.ID == "" {
		task.ID = uuid.New().String()
	}
	now := time.Now().UTC()
	task.CreatedAt = now
	task.UpdatedAt = now

	configJSON, err := task.MarshalScheduleConfig()
	if err != nil {
		return fmt.Errorf("marshaling schedule config: %w", err)
	}

	ctx := context.Background()
	_, err = s.db.ExecContext(ctx, `
		INSERT INTO scheduled_tasks
			(id, name, description, prompt, agent_slug, working_directory, model,
			 settings_profile_id, timeout_minutes, schedule_type, schedule_config,
			 stop_after_count, stop_after_time, status, run_count, last_run_at,
			 last_run_status, next_run_at, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		task.ID, task.Name, task.Description, task.Prompt, task.AgentSlug,
		task.WorkingDirectory, task.Model, task.SettingsProfileID, task.TimeoutMinutes,
		task.ScheduleType, configJSON, task.StopAfterCount, task.StopAfterTime,
		task.Status, task.RunCount, task.LastRunAt, task.LastRunStatus, task.NextRunAt,
		task.CreatedAt, task.UpdatedAt,
	)
	if err != nil {
		return fmt.Errorf("creating task: %w", err)
	}
	return nil
}

// UpdateTask updates an existing scheduled task.
func (s *SQLiteTaskStore) UpdateTask(task *ScheduledTask) error {
	task.UpdatedAt = time.Now().UTC()

	configJSON, err := task.MarshalScheduleConfig()
	if err != nil {
		return fmt.Errorf("marshaling schedule config: %w", err)
	}

	ctx := context.Background()
	res, err := s.db.ExecContext(ctx, `
		UPDATE scheduled_tasks SET
			name = ?, description = ?, prompt = ?, agent_slug = ?,
			working_directory = ?, model = ?, settings_profile_id = ?,
			timeout_minutes = ?, schedule_type = ?, schedule_config = ?,
			stop_after_count = ?, stop_after_time = ?, status = ?,
			run_count = ?, last_run_at = ?, last_run_status = ?,
			next_run_at = ?, updated_at = ?
		WHERE id = ?`,
		task.Name, task.Description, task.Prompt, task.AgentSlug,
		task.WorkingDirectory, task.Model, task.SettingsProfileID,
		task.TimeoutMinutes, task.ScheduleType, configJSON,
		task.StopAfterCount, task.StopAfterTime, task.Status,
		task.RunCount, task.LastRunAt, task.LastRunStatus,
		task.NextRunAt, task.UpdatedAt, task.ID,
	)
	if err != nil {
		return fmt.Errorf("updating task %q: %w", task.ID, err)
	}
	n, rowErr := res.RowsAffected()
	if rowErr != nil {
		return fmt.Errorf("checking rows affected for task %q: %w", task.ID, rowErr)
	}
	if n == 0 {
		return fmt.Errorf("task %q not found", task.ID)
	}
	return nil
}

// DeleteTask deletes a scheduled task and its job history (via CASCADE).
func (s *SQLiteTaskStore) DeleteTask(id string) error {
	ctx := context.Background()
	res, err := s.db.ExecContext(ctx, "DELETE FROM scheduled_tasks WHERE id = ?", id)
	if err != nil {
		return fmt.Errorf("deleting task %q: %w", id, err)
	}
	n, rowErr := res.RowsAffected()
	if rowErr != nil {
		return fmt.Errorf("checking rows affected for task %q: %w", id, rowErr)
	}
	if n == 0 {
		return fmt.Errorf("task %q not found", id)
	}
	return nil
}

// ListJobHistory returns job history entries for a specific task.
func (s *SQLiteTaskStore) ListJobHistory(taskID string, limit int) ([]*JobHistory, error) {
	ctx := context.Background()
	rows, err := s.db.QueryContext(ctx, `
		SELECT id, task_id, task_name, agent_slug, status, started_at, finished_at,
		       duration_ms, chat_session_id, model, prompt_preview, error_message,
		       total_input_tokens, total_output_tokens,
		       total_cache_creation_tokens, total_cache_read_tokens
		FROM job_history
		WHERE task_id = ?
		ORDER BY started_at DESC
		LIMIT ?`, taskID, limit)
	if err != nil {
		return nil, fmt.Errorf("listing job history for task %q: %w", taskID, err)
	}
	defer rows.Close() //nolint:errcheck
	return scanJobHistoryRows(rows)
}

// ListAllJobHistory returns all job history entries with pagination.
func (s *SQLiteTaskStore) ListAllJobHistory(limit, offset int) ([]*JobHistory, error) {
	ctx := context.Background()
	rows, err := s.db.QueryContext(ctx, `
		SELECT id, task_id, task_name, agent_slug, status, started_at, finished_at,
		       duration_ms, chat_session_id, model, prompt_preview, error_message,
		       total_input_tokens, total_output_tokens,
		       total_cache_creation_tokens, total_cache_read_tokens
		FROM job_history
		ORDER BY started_at DESC
		LIMIT ? OFFSET ?`, limit, offset)
	if err != nil {
		return nil, fmt.Errorf("listing all job history: %w", err)
	}
	defer rows.Close() //nolint:errcheck
	return scanJobHistoryRows(rows)
}

// GetJobHistory returns a single job history entry by ID, or nil if not found.
func (s *SQLiteTaskStore) GetJobHistory(id string) (*JobHistory, error) {
	ctx := context.Background()
	row := s.db.QueryRowContext(ctx, `
		SELECT id, task_id, task_name, agent_slug, status, started_at, finished_at,
		       duration_ms, chat_session_id, model, prompt_preview, error_message,
		       total_input_tokens, total_output_tokens,
		       total_cache_creation_tokens, total_cache_read_tokens
		FROM job_history WHERE id = ?`, id)

	jh, err := scanJobHistoryRow(row)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("getting job history %q: %w", id, err)
	}
	return jh, nil
}

// CreateJobHistory inserts a new job history record.
func (s *SQLiteTaskStore) CreateJobHistory(jh *JobHistory) error {
	if jh.ID == "" {
		jh.ID = uuid.New().String()
	}

	ctx := context.Background()
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO job_history
			(id, task_id, task_name, agent_slug, status, started_at, finished_at,
			 duration_ms, chat_session_id, model, prompt_preview, error_message,
			 total_input_tokens, total_output_tokens,
			 total_cache_creation_tokens, total_cache_read_tokens)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		jh.ID, jh.TaskID, jh.TaskName, jh.AgentSlug, jh.Status,
		jh.StartedAt, jh.FinishedAt, jh.DurationMS, jh.ChatSessionID,
		jh.Model, jh.PromptPreview, jh.ErrorMessage,
		jh.TotalInputTokens, jh.TotalOutputTokens,
		jh.TotalCacheCreationTokens, jh.TotalCacheReadTokens,
	)
	if err != nil {
		return fmt.Errorf("creating job history: %w", err)
	}
	return nil
}

// UpdateJobHistory updates an existing job history record.
func (s *SQLiteTaskStore) UpdateJobHistory(jh *JobHistory) error {
	ctx := context.Background()
	_, err := s.db.ExecContext(ctx, `
		UPDATE job_history SET
			status = ?, finished_at = ?, duration_ms = ?, chat_session_id = ?,
			error_message = ?, total_input_tokens = ?, total_output_tokens = ?,
			total_cache_creation_tokens = ?, total_cache_read_tokens = ?
		WHERE id = ?`,
		jh.Status, jh.FinishedAt, jh.DurationMS, jh.ChatSessionID,
		jh.ErrorMessage, jh.TotalInputTokens, jh.TotalOutputTokens,
		jh.TotalCacheCreationTokens, jh.TotalCacheReadTokens, jh.ID,
	)
	if err != nil {
		return fmt.Errorf("updating job history %q: %w", jh.ID, err)
	}
	return nil
}

// scanScheduledTask scans a scheduled task from a row set.
func scanScheduledTask(rows *sql.Rows) (*ScheduledTask, error) {
	t := &ScheduledTask{}
	var configJSON string
	var stopAfterTime sql.NullTime
	var lastRunAt sql.NullTime
	var nextRunAt sql.NullTime

	err := rows.Scan(
		&t.ID, &t.Name, &t.Description, &t.Prompt, &t.AgentSlug,
		&t.WorkingDirectory, &t.Model, &t.SettingsProfileID, &t.TimeoutMinutes,
		&t.ScheduleType, &configJSON, &t.StopAfterCount, &stopAfterTime,
		&t.Status, &t.RunCount, &lastRunAt, &t.LastRunStatus, &nextRunAt,
		&t.CreatedAt, &t.UpdatedAt,
	)
	if err != nil {
		return nil, fmt.Errorf("scanning task: %w", err)
	}

	if err := json.Unmarshal([]byte(configJSON), &t.ScheduleConfig); err != nil {
		return nil, fmt.Errorf("unmarshaling schedule config: %w", err)
	}
	if stopAfterTime.Valid {
		t.StopAfterTime = &stopAfterTime.Time
	}
	if lastRunAt.Valid {
		t.LastRunAt = &lastRunAt.Time
	}
	if nextRunAt.Valid {
		t.NextRunAt = &nextRunAt.Time
	}
	return t, nil
}

// scanJobHistoryRows scans multiple job history rows.
func scanJobHistoryRows(rows *sql.Rows) ([]*JobHistory, error) {
	results := make([]*JobHistory, 0)
	for rows.Next() {
		jh := &JobHistory{}
		var finishedAt sql.NullTime
		err := rows.Scan(
			&jh.ID, &jh.TaskID, &jh.TaskName, &jh.AgentSlug, &jh.Status,
			&jh.StartedAt, &finishedAt, &jh.DurationMS, &jh.ChatSessionID,
			&jh.Model, &jh.PromptPreview, &jh.ErrorMessage,
			&jh.TotalInputTokens, &jh.TotalOutputTokens,
			&jh.TotalCacheCreationTokens, &jh.TotalCacheReadTokens,
		)
		if err != nil {
			return nil, fmt.Errorf("scanning job history: %w", err)
		}
		if finishedAt.Valid {
			jh.FinishedAt = &finishedAt.Time
		}
		results = append(results, jh)
	}
	return results, rows.Err()
}

// scanJobHistoryRow scans a single job history row.
func scanJobHistoryRow(row *sql.Row) (*JobHistory, error) {
	jh := &JobHistory{}
	var finishedAt sql.NullTime
	err := row.Scan(
		&jh.ID, &jh.TaskID, &jh.TaskName, &jh.AgentSlug, &jh.Status,
		&jh.StartedAt, &finishedAt, &jh.DurationMS, &jh.ChatSessionID,
		&jh.Model, &jh.PromptPreview, &jh.ErrorMessage,
		&jh.TotalInputTokens, &jh.TotalOutputTokens,
		&jh.TotalCacheCreationTokens, &jh.TotalCacheReadTokens,
	)
	if err != nil {
		return nil, err
	}
	if finishedAt.Valid {
		jh.FinishedAt = &finishedAt.Time
	}
	return jh, nil
}
