package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"strings"
	"time"
)

// InsightRecord is the storage-layer representation of a session insight row.
// It mirrors the database schema without importing the claudesessions package,
// avoiding a circular dependency between storage and claudesessions.
type InsightRecord struct {
	SessionID        string
	ProcessorVersion int
	ScannedAt        time.Time

	TurnCount       int
	StepsPerTurnAvg float64

	AutonomyScore float64

	ToolCallsTotal int
	ToolBreakdown  map[string]int // stored as JSON in DB
	ToolErrorRate  float64

	TotalDurationMs int64
	ThinkingTimeMs  int64

	CacheHitRate     float64
	TokensPerTurnAvg float64
	CostEstimateUSD  float64

	ToolErrorCount int
	HasErrors      bool

	MaxConsecutiveToolCalls int
	LongestAutonomousChain  int

	AvgUserResponseTimeMs   float64
	AvgClaudeResponseTimeMs float64

	SessionType string
}

// SQLiteSessionInsightsStore persists per-session insight records in SQLite.
type SQLiteSessionInsightsStore struct {
	db *sql.DB
}

// NewSQLiteSessionInsightsStore returns a store backed by the given database.
func NewSQLiteSessionInsightsStore(db *sql.DB) *SQLiteSessionInsightsStore {
	return &SQLiteSessionInsightsStore{db: db}
}

// Upsert inserts or replaces the insight record for a session.
func (s *SQLiteSessionInsightsStore) Upsert(ctx context.Context, r InsightRecord) error {
	ctx, end := withStorageSpan(ctx, "upsert", "session_insights")
	var err error
	defer func() { end(err) }()

	args, err := insightArgs(r)
	if err != nil {
		return err
	}
	_, err = s.db.ExecContext(ctx, insightUpsertSQL, args...)
	return err
}

// insightArgs serializes an InsightRecord into the ordered SQL parameter slice
// for insightUpsertSQL.
func insightArgs(r InsightRecord) ([]any, error) {
	breakdown, err := json.Marshal(r.ToolBreakdown)
	if err != nil {
		return nil, fmt.Errorf("marshaling tool_breakdown: %w", err)
	}
	hasErrors := 0
	if r.HasErrors {
		hasErrors = 1
	}
	return []any{
		r.SessionID, r.ProcessorVersion, r.ScannedAt.UTC().Format(time.RFC3339),
		r.TurnCount, r.StepsPerTurnAvg, r.AutonomyScore,
		r.ToolCallsTotal, string(breakdown), r.ToolErrorRate,
		r.TotalDurationMs, r.ThinkingTimeMs,
		r.CacheHitRate, r.TokensPerTurnAvg, r.CostEstimateUSD,
		r.ToolErrorCount, hasErrors,
		r.MaxConsecutiveToolCalls, r.LongestAutonomousChain,
		r.AvgUserResponseTimeMs, r.AvgClaudeResponseTimeMs,
		r.SessionType,
	}, nil
}

const insightUpsertSQL = `
INSERT INTO session_insights (
    session_id, processor_version, scanned_at,
    turn_count, steps_per_turn_avg, autonomy_score,
    tool_calls_total, tool_breakdown, tool_error_rate,
    total_duration_ms, thinking_time_ms,
    cache_hit_rate, tokens_per_turn_avg, cost_estimate_usd,
    tool_error_count, has_errors,
    max_consecutive_tool_calls, longest_autonomous_chain,
    avg_user_response_time_ms, avg_claude_response_time_ms,
    session_type
) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
ON CONFLICT(session_id) DO UPDATE SET
    processor_version           = excluded.processor_version,
    scanned_at                  = excluded.scanned_at,
    turn_count                  = excluded.turn_count,
    steps_per_turn_avg          = excluded.steps_per_turn_avg,
    autonomy_score              = excluded.autonomy_score,
    tool_calls_total            = excluded.tool_calls_total,
    tool_breakdown              = excluded.tool_breakdown,
    tool_error_rate             = excluded.tool_error_rate,
    total_duration_ms           = excluded.total_duration_ms,
    thinking_time_ms            = excluded.thinking_time_ms,
    cache_hit_rate              = excluded.cache_hit_rate,
    tokens_per_turn_avg         = excluded.tokens_per_turn_avg,
    cost_estimate_usd           = excluded.cost_estimate_usd,
    tool_error_count            = excluded.tool_error_count,
    has_errors                  = excluded.has_errors,
    max_consecutive_tool_calls  = excluded.max_consecutive_tool_calls,
    longest_autonomous_chain    = excluded.longest_autonomous_chain,
    avg_user_response_time_ms   = excluded.avg_user_response_time_ms,
    avg_claude_response_time_ms = excluded.avg_claude_response_time_ms,
    session_type                = excluded.session_type`

// Get retrieves the insight for a single session. Returns nil, nil when not found.
func (s *SQLiteSessionInsightsStore) Get(ctx context.Context, sessionID string) (*InsightRecord, error) {
	ctx, end := withStorageSpan(ctx, "get", "session_insights")
	var err error
	defer func() { end(err) }()

	row := s.db.QueryRowContext(ctx, insightSelectCols+` WHERE session_id = ?`, sessionID)
	r, err := scanInsightRecord(row)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	return r, err
}

// GetMany retrieves insights for the given session IDs. Missing sessions are silently omitted.
func (s *SQLiteSessionInsightsStore) GetMany(ctx context.Context, sessionIDs []string) ([]*InsightRecord, error) {
	if len(sessionIDs) == 0 {
		return nil, nil
	}

	ctx, end := withStorageSpan(ctx, "get_many", "session_insights")
	var err error
	defer func() { end(err) }()

	placeholders := make([]string, len(sessionIDs))
	args := make([]any, len(sessionIDs))
	for i, id := range sessionIDs {
		placeholders[i] = "?"
		args[i] = id
	}

	//nolint:gosec // placeholders are generated from fixed pattern, not user input
	query := insightSelectCols + ` WHERE session_id IN (` + strings.Join(placeholders, ",") + `)`
	rows, err := s.db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			err = cerr
		}
	}()

	var results []*InsightRecord
	for rows.Next() {
		r, scanErr := scanInsightRecord(rows)
		if scanErr != nil {
			return nil, scanErr
		}
		results = append(results, r)
	}
	return results, rows.Err()
}

// GetAll retrieves all insight records (used when no IDs are specified in summary).
func (s *SQLiteSessionInsightsStore) GetAll(ctx context.Context) ([]*InsightRecord, error) {
	ctx, end := withStorageSpan(ctx, "get_all", "session_insights")
	var err error
	defer func() { end(err) }()

	rows, err := s.db.QueryContext(ctx, insightSelectCols)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			err = cerr
		}
	}()

	var results []*InsightRecord
	for rows.Next() {
		r, scanErr := scanInsightRecord(rows)
		if scanErr != nil {
			return nil, scanErr
		}
		results = append(results, r)
	}
	return results, rows.Err()
}

// NeedsProcessing returns session IDs from claude_session_cache that either
// have no insight row or whose insight has processor_version < version.
func (s *SQLiteSessionInsightsStore) NeedsProcessing(ctx context.Context, version int) ([]string, error) {
	ctx, end := withStorageSpan(ctx, "needs_processing", "session_insights")
	var err error
	defer func() { end(err) }()

	rows, err := s.db.QueryContext(ctx, `
SELECT DISTINCT c.session_id
FROM claude_session_cache c
LEFT JOIN session_insights i ON c.session_id = i.session_id
WHERE i.session_id IS NULL OR i.processor_version < ?`, version)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			err = cerr
		}
	}()

	var ids []string
	for rows.Next() {
		var id string
		if scanErr := rows.Scan(&id); scanErr != nil {
			return nil, scanErr
		}
		ids = append(ids, id)
	}
	return ids, rows.Err()
}

const insightSelectCols = `
SELECT session_id, processor_version, scanned_at,
       turn_count, steps_per_turn_avg, autonomy_score,
       tool_calls_total, tool_breakdown, tool_error_rate,
       total_duration_ms, thinking_time_ms,
       cache_hit_rate, tokens_per_turn_avg, cost_estimate_usd,
       tool_error_count, has_errors,
       max_consecutive_tool_calls, longest_autonomous_chain,
       avg_user_response_time_ms, avg_claude_response_time_ms,
       session_type
FROM session_insights`

// rowScanner is satisfied by both *sql.Row and *sql.Rows.
type rowScanner interface {
	Scan(dest ...any) error
}

func scanInsightRecord(row rowScanner) (*InsightRecord, error) {
	var (
		r             InsightRecord
		scannedAt     string
		toolBreakdown string
		hasErrors     int
	)

	err := row.Scan(
		&r.SessionID,
		&r.ProcessorVersion,
		&scannedAt,
		&r.TurnCount,
		&r.StepsPerTurnAvg,
		&r.AutonomyScore,
		&r.ToolCallsTotal,
		&toolBreakdown,
		&r.ToolErrorRate,
		&r.TotalDurationMs,
		&r.ThinkingTimeMs,
		&r.CacheHitRate,
		&r.TokensPerTurnAvg,
		&r.CostEstimateUSD,
		&r.ToolErrorCount,
		&hasErrors,
		&r.MaxConsecutiveToolCalls,
		&r.LongestAutonomousChain,
		&r.AvgUserResponseTimeMs,
		&r.AvgClaudeResponseTimeMs,
		&r.SessionType,
	)
	if err != nil {
		return nil, err
	}

	r.HasErrors = hasErrors != 0
	if t, parseErr := time.Parse(time.RFC3339, scannedAt); parseErr == nil {
		r.ScannedAt = t
	}

	r.ToolBreakdown = make(map[string]int)
	if toolBreakdown != "" && toolBreakdown != "{}" {
		if unmarshalErr := json.Unmarshal([]byte(toolBreakdown), &r.ToolBreakdown); unmarshalErr != nil {
			// Non-fatal: leave breakdown as empty map if JSON is malformed.
			r.ToolBreakdown = make(map[string]int)
		}
	}

	return &r, nil
}
