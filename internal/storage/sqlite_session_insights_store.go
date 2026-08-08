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

	// Attribution breakdowns, each stored as JSON. They count tool calls, so
	// sum(SkillBreakdown) + UnattributedCalls == ToolCallsTotal.
	SkillBreakdown     map[string]int
	PluginBreakdown    map[string]int
	McpServerBreakdown map[string]int
	McpToolBreakdown   map[string]int
	EffortBreakdown    map[string]int
	UnattributedCalls  int

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
	breakdown, err := marshalCounts("tool_breakdown", r.ToolBreakdown)
	if err != nil {
		return nil, err
	}
	skills, err := marshalCounts("skill_breakdown", r.SkillBreakdown)
	if err != nil {
		return nil, err
	}
	plugins, err := marshalCounts("plugin_breakdown", r.PluginBreakdown)
	if err != nil {
		return nil, err
	}
	mcpServers, err := marshalCounts("mcp_server_breakdown", r.McpServerBreakdown)
	if err != nil {
		return nil, err
	}
	mcpTools, err := marshalCounts("mcp_tool_breakdown", r.McpToolBreakdown)
	if err != nil {
		return nil, err
	}
	efforts, err := marshalCounts("effort_breakdown", r.EffortBreakdown)
	if err != nil {
		return nil, err
	}
	hasErrors := 0
	if r.HasErrors {
		hasErrors = 1
	}
	return []any{
		r.SessionID, r.ProcessorVersion, r.ScannedAt.UTC().Format(time.RFC3339),
		r.TurnCount, r.StepsPerTurnAvg, r.AutonomyScore,
		r.ToolCallsTotal, breakdown, r.ToolErrorRate,
		r.TotalDurationMs, r.ThinkingTimeMs,
		r.CacheHitRate, r.TokensPerTurnAvg, r.CostEstimateUSD,
		r.ToolErrorCount, hasErrors,
		r.MaxConsecutiveToolCalls, r.LongestAutonomousChain,
		r.AvgUserResponseTimeMs, r.AvgClaudeResponseTimeMs,
		r.SessionType,
		skills, plugins, mcpServers, mcpTools, efforts, r.UnattributedCalls,
	}, nil
}

// marshalCounts serializes a breakdown map for its TEXT column. A nil map
// stores "{}" rather than "null", so readers never have to special-case it.
func marshalCounts(column string, counts map[string]int) (string, error) {
	if counts == nil {
		return "{}", nil
	}
	b, err := json.Marshal(counts)
	if err != nil {
		return "", fmt.Errorf("marshaling %s: %w", column, err)
	}
	return string(b), nil
}

// unmarshalCounts is the inverse. It always returns a non-nil map, matching
// what ToolBreakdown hands back, so the whole record uses one empty convention.
// An empty or malformed column yields an empty map rather than an error: a
// breakdown is a derived convenience, and losing the whole insight row over one
// bad blob would be worse than losing the blob.
func unmarshalCounts(raw string) map[string]int {
	counts := make(map[string]int)
	if raw == "" || raw == "{}" {
		return counts
	}
	if err := json.Unmarshal([]byte(raw), &counts); err != nil {
		return make(map[string]int)
	}
	return counts
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
    session_type,
    skill_breakdown, plugin_breakdown, mcp_server_breakdown,
    mcp_tool_breakdown, effort_breakdown, unattributed_calls
) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
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
    session_type                = excluded.session_type,
    skill_breakdown             = excluded.skill_breakdown,
    plugin_breakdown            = excluded.plugin_breakdown,
    mcp_server_breakdown        = excluded.mcp_server_breakdown,
    mcp_tool_breakdown          = excluded.mcp_tool_breakdown,
    effort_breakdown            = excluded.effort_breakdown,
    unattributed_calls          = excluded.unattributed_calls`

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

// InsightAggregateSummary holds SQL-computed aggregate statistics for session insights.
// Scalar fields are computed in a single GROUP BY query; ToolBreakdowns contains the
// raw JSON tool_breakdown strings for top-tool aggregation in the caller.
type InsightAggregateSummary struct {
	TotalSessions        int
	AvgAutonomyScore     float64
	AvgTurnCount         float64
	AvgToolCallsTotal    float64
	TotalCostEstimateUSD float64
	AvgCacheHitRate      float64
	AvgTotalDurationMs   float64
	SessionsWithErrors   int
	// TotalToolCalls and UnattributedCalls give the breakdowns a denominator:
	// without them a "top skills" panel silently omits every built-in call.
	TotalToolCalls    int
	UnattributedCalls int
	ToolBreakdowns    []string // raw JSON per session for top-tool aggregation
	// Raw JSON per session for the attribution breakdowns, merged by the caller
	// exactly as ToolBreakdowns is.
	SkillBreakdowns     []string
	PluginBreakdowns    []string
	McpServerBreakdowns []string
}

// GetAggregateSummary computes aggregated insight statistics using SQL aggregation
// for scalars and fetches only the tool_breakdown JSON column for top-tool computation.
// If sessionIDs is non-empty, results are filtered to those sessions.
// from and to are inclusive date boundaries applied against claude_session_cache.start_time;
// nil means unbounded.
func (s *SQLiteSessionInsightsStore) GetAggregateSummary(
	ctx context.Context, sessionIDs []string, from, to *time.Time,
) (*InsightAggregateSummary, error) {
	ctx, end := withStorageSpan(ctx, "get_aggregate_summary", "session_insights")
	var err error
	defer func() { end(err) }()

	where, args := insightWhereClause(sessionIDs, from, to)
	summary, err := s.queryAggregateScalars(ctx, where, args)
	if err != nil || summary.TotalSessions == 0 {
		return summary, err
	}

	for _, col := range []struct {
		name string
		into *[]string
	}{
		{"tool_breakdown", &summary.ToolBreakdowns},
		{"skill_breakdown", &summary.SkillBreakdowns},
		{"plugin_breakdown", &summary.PluginBreakdowns},
		{"mcp_server_breakdown", &summary.McpServerBreakdowns},
	} {
		values, qErr := s.queryBreakdowns(ctx, col.name, where, args)
		if qErr != nil {
			err = qErr
			return summary, err
		}
		*col.into = values
	}
	return summary, nil
}

// insightWhereClause builds a WHERE clause that optionally filters by session ID list
// and/or by session start_time range (via a subquery on claude_session_cache).
// All predicates use parameterised placeholders to prevent SQL injection.
func insightWhereClause(sessionIDs []string, from, to *time.Time) (string, []any) {
	var clauses []string
	var args []any

	if len(sessionIDs) > 0 {
		placeholders := make([]string, len(sessionIDs))
		for i, id := range sessionIDs {
			placeholders[i] = "?"
			args = append(args, id)
		}
		//nolint:gosec // placeholders are generated from fixed pattern, not user input
		clauses = append(clauses, "session_id IN ("+strings.Join(placeholders, ",")+")")
	}

	if from != nil || to != nil {
		// Use a subquery so we never JOIN and accidentally duplicate rows when a
		// session_id appears in multiple project_path entries.
		sub, subArgs := dateRangeSubquery(from, to)
		clauses = append(clauses, "session_id IN ("+sub+")")
		args = append(args, subArgs...)
	}

	if len(clauses) == 0 {
		return "", nil
	}
	return " WHERE " + strings.Join(clauses, " AND "), args
}

// dateRangeSubquery returns a SELECT subquery that returns session_ids from
// claude_session_cache whose start_time falls within [from, to] (inclusive).
// Nil boundaries are treated as unbounded.
func dateRangeSubquery(from, to *time.Time) (string, []any) {
	if from == nil && to == nil {
		return "SELECT DISTINCT session_id FROM claude_session_cache", nil
	}
	var conds []string
	var args []any
	if from != nil {
		conds = append(conds, "start_time >= ?")
		args = append(args, from.UTC().Format(time.RFC3339))
	}
	if to != nil {
		// Add one day so "to" is inclusive for day-level comparisons.
		end := to.UTC().Add(24 * time.Hour)
		conds = append(conds, "start_time < ?")
		args = append(args, end.Format(time.RFC3339))
	}
	//nolint:gosec // conds are hard-coded string literals, not user input
	sub := "SELECT DISTINCT session_id FROM claude_session_cache WHERE " + strings.Join(conds, " AND ")
	return sub, args
}

const insightAggregateSQL = `SELECT
	COUNT(*),
	COALESCE(AVG(autonomy_score), 0),
	COALESCE(AVG(turn_count), 0),
	COALESCE(AVG(tool_calls_total), 0),
	COALESCE(SUM(cost_estimate_usd), 0),
	COALESCE(AVG(cache_hit_rate), 0),
	COALESCE(AVG(total_duration_ms), 0),
	COALESCE(SUM(has_errors), 0),
	COALESCE(SUM(tool_calls_total), 0),
	COALESCE(SUM(unattributed_calls), 0)
FROM session_insights`

func (s *SQLiteSessionInsightsStore) queryAggregateScalars(
	ctx context.Context, where string, args []any,
) (*InsightAggregateSummary, error) {
	// Aggregate scalar fields in SQL — avoids loading all rows into Go memory.
	//nolint:gosec // where clause uses parameterized placeholders only
	row := s.db.QueryRowContext(ctx, insightAggregateSQL+where, args...)
	summary := &InsightAggregateSummary{}
	err := row.Scan(
		&summary.TotalSessions,
		&summary.AvgAutonomyScore,
		&summary.AvgTurnCount,
		&summary.AvgToolCallsTotal,
		&summary.TotalCostEstimateUSD,
		&summary.AvgCacheHitRate,
		&summary.AvgTotalDurationMs,
		&summary.SessionsWithErrors,
		&summary.TotalToolCalls,
		&summary.UnattributedCalls,
	)
	return summary, err
}

// queryBreakdowns fetches one breakdown JSON column per matching session, for
// the caller to merge. column is chosen from a fixed set by GetAggregateSummary
// and is never user input.
func (s *SQLiteSessionInsightsStore) queryBreakdowns(
	ctx context.Context, column, where string, args []any,
) ([]string, error) {
	//nolint:gosec // column comes from a fixed literal set; where uses placeholders only
	rows, err := s.db.QueryContext(ctx, "SELECT "+column+" FROM session_insights"+where, args...)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			err = cerr
		}
	}()

	var breakdowns []string
	for rows.Next() {
		var tb string
		if scanErr := rows.Scan(&tb); scanErr != nil {
			return nil, scanErr
		}
		if tb != "" && tb != "{}" {
			breakdowns = append(breakdowns, tb)
		}
	}
	return breakdowns, rows.Err()
}

// SessionToProcess pairs a session ID with its JSONL file path for processing.
type SessionToProcess struct {
	SessionID string
	FilePath  string
}

// NeedsProcessing returns sessions from claude_session_cache that either
// have no insight row or whose insight has processor_version < version.
// The file_path is included in the result so callers do not need a separate
// filesystem walk to locate the JSONL file.
func (s *SQLiteSessionInsightsStore) NeedsProcessing(
	ctx context.Context, version int,
) ([]SessionToProcess, error) {
	ctx, end := withStorageSpan(ctx, "needs_processing", "session_insights")
	var err error
	defer func() { end(err) }()

	rows, err := s.db.QueryContext(ctx, `
SELECT DISTINCT c.session_id, c.file_path
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

	var sessions []SessionToProcess
	for rows.Next() {
		var s SessionToProcess
		if scanErr := rows.Scan(&s.SessionID, &s.FilePath); scanErr != nil {
			return nil, scanErr
		}
		sessions = append(sessions, s)
	}
	return sessions, rows.Err()
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
       session_type,
       skill_breakdown, plugin_breakdown, mcp_server_breakdown,
       mcp_tool_breakdown, effort_breakdown, unattributed_calls
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
		b             attributionColumns
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
		&b.skills,
		&b.plugins,
		&b.mcpServers,
		&b.mcpTools,
		&b.efforts,
		&r.UnattributedCalls,
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
	r.SkillBreakdown = unmarshalCounts(b.skills)
	r.PluginBreakdown = unmarshalCounts(b.plugins)
	r.McpServerBreakdown = unmarshalCounts(b.mcpServers)
	r.McpToolBreakdown = unmarshalCounts(b.mcpTools)
	r.EffortBreakdown = unmarshalCounts(b.efforts)

	return &r, nil
}

// attributionColumns holds the raw JSON of the attribution breakdown columns
// between Scan and decoding, keeping the scan argument list manageable.
type attributionColumns struct {
	skills     string
	plugins    string
	mcpServers string
	mcpTools   string
	efforts    string
}
