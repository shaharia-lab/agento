package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
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
	AgentBreakdown     map[string]int
	UnattributedCalls  int

	TotalDurationMs     int64
	ActiveDurationMs    int64
	ClaudeWorkingTimeMs int64

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
	agents, err := marshalCounts("agent_breakdown", r.AgentBreakdown)
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
		r.TotalDurationMs, r.ActiveDurationMs, r.ClaudeWorkingTimeMs,
		r.CacheHitRate, r.TokensPerTurnAvg, r.CostEstimateUSD,
		r.ToolErrorCount, hasErrors,
		r.MaxConsecutiveToolCalls, r.LongestAutonomousChain,
		r.AvgUserResponseTimeMs, r.AvgClaudeResponseTimeMs,
		r.SessionType,
		skills, plugins, mcpServers, mcpTools, efforts, r.UnattributedCalls,
		agents,
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
    total_duration_ms, active_duration_ms, claude_working_time_ms,
    cache_hit_rate, tokens_per_turn_avg, cost_estimate_usd,
    tool_error_count, has_errors,
    max_consecutive_tool_calls, longest_autonomous_chain,
    avg_user_response_time_ms, avg_claude_response_time_ms,
    session_type,
    skill_breakdown, plugin_breakdown, mcp_server_breakdown,
    mcp_tool_breakdown, effort_breakdown, unattributed_calls,
    agent_breakdown
) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
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
    active_duration_ms          = excluded.active_duration_ms,
    claude_working_time_ms      = excluded.claude_working_time_ms,
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
    unattributed_calls          = excluded.unattributed_calls,
    agent_breakdown             = excluded.agent_breakdown`

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

	// The same json_each binding GetAggregateSummary uses, for the same reason:
	// one placeholder per ID overflows SQLite's variable limit on a large set,
	// and one idiom per file beats two answers to one question.
	where, args, err := insightWhereClause(sessionIDs)
	if err != nil {
		return nil, err
	}

	//nolint:gosec // the clause is a fixed string; IDs travel as one bound parameter
	rows, err := s.db.QueryContext(ctx, insightSelectCols+where, args...)
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
	// AvgActiveDurationMs averages the idle-capped figure; see the migration-24
	// comment for why the raw span mean is not what dashboards should show.
	AvgActiveDurationMs float64
	SessionsWithErrors  int
	// TotalToolErrors is the summed tool_error_count, the numerator for an
	// errors-per-100-tool-calls rate. SessionsWithErrors alone cannot express
	// one: it counts sessions, not errors, so a session with a single failing
	// grep and one with fifty broken commands are the same number.
	TotalToolErrors int
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
	McpToolBreakdowns   []string
	EffortBreakdowns    []string
	AgentBreakdowns     []string
}

// GetAggregateSummary computes aggregated insight statistics over exactly the
// given sessions, using SQL aggregation for scalars and fetching only the
// breakdown JSON columns for merging by the caller.
//
// sessionIDs is the complete set to include, never a hint: an empty set yields
// a zero summary rather than "everything". Windowing happens before this call,
// in claudesessions.FilterSessions, so the insights summary and the analytics
// report cover the same sessions by construction rather than by two SQL
// predicates that agreed until one of them was edited.
//
// Filtering here by date would not even be reliable: the DATETIME columns hold
// Go's time.Time.String() rendering ("2024-03-15 12:00:00 +0000 UTC"), so
// comparing them against an RFC3339 bound — as this did — compares ' ' with 'T'
// at index 10 and silently misplaces every row whose date equals a boundary's.
func (s *SQLiteSessionInsightsStore) GetAggregateSummary(
	ctx context.Context, sessionIDs []string,
) (*InsightAggregateSummary, error) {
	ctx, end := withStorageSpan(ctx, "get_aggregate_summary", "session_insights")
	var err error
	defer func() { end(err) }()

	if len(sessionIDs) == 0 {
		return &InsightAggregateSummary{}, nil
	}

	where, args, err := insightWhereClause(sessionIDs)
	if err != nil {
		return nil, err
	}
	summary, err := s.queryAggregateScalars(ctx, where, args)
	if err != nil || summary.TotalSessions == 0 {
		return summary, err
	}

	if err = s.queryAllBreakdowns(ctx, where, args, summary); err != nil {
		return summary, err
	}
	return summary, nil
}

// insightWhereClause restricts a query to the given session IDs.
//
// The set travels as a single JSON-array parameter expanded by json_each rather
// than as one placeholder per ID. A window can legitimately contain thousands of
// sessions, and a placeholder per ID would hit SQLite's variable limit on
// exactly the corpora this feature exists for — while the bind stays fully
// parameterised, so no ID is ever interpolated into SQL.
func insightWhereClause(sessionIDs []string) (string, []any, error) {
	if sessionIDs == nil {
		// Marshaling a nil slice yields "null", and json_each('null') produces a
		// single NULL row that matches nothing only because NULL comparisons are
		// never true. Both callers already refuse an empty set before getting
		// here; encoding "[]" makes the clause correct on its own rather than by
		// their good behavior.
		sessionIDs = []string{}
	}
	ids, err := json.Marshal(sessionIDs)
	if err != nil {
		return "", nil, fmt.Errorf("marshaling session id filter: %w", err)
	}
	return " WHERE session_id IN (SELECT value FROM json_each(?))", []any{string(ids)}, nil
}

const insightAggregateSQL = `SELECT
	COUNT(*),
	COALESCE(AVG(autonomy_score), 0),
	COALESCE(AVG(turn_count), 0),
	COALESCE(AVG(tool_calls_total), 0),
	COALESCE(SUM(cost_estimate_usd), 0),
	COALESCE(AVG(cache_hit_rate), 0),
	COALESCE(AVG(total_duration_ms), 0),
	COALESCE(AVG(active_duration_ms), 0),
	COALESCE(SUM(has_errors), 0),
	COALESCE(SUM(tool_calls_total), 0),
	COALESCE(SUM(unattributed_calls), 0),
	COALESCE(SUM(tool_error_count), 0)
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
		&summary.AvgActiveDurationMs,
		&summary.SessionsWithErrors,
		&summary.TotalToolCalls,
		&summary.UnattributedCalls,
		&summary.TotalToolErrors,
	)
	return summary, err
}

const insightBreakdownSQL = `SELECT tool_breakdown, skill_breakdown, plugin_breakdown,
       mcp_server_breakdown, mcp_tool_breakdown, effort_breakdown, agent_breakdown
FROM session_insights`

// queryAllBreakdowns fetches every breakdown JSON column in a single scan and
// fills the summary's four slices, for the caller to merge. One query rather
// than one per column: the row-scan cost then grows with the corpus only, not
// with how many breakdown dimensions the feature has accumulated.
//
// Each column is filtered independently — a row with tools but no skills
// contributes to ToolBreakdowns alone. Skipping the whole row when any column
// is empty would silently drop real data from the merged totals.
func (s *SQLiteSessionInsightsStore) queryAllBreakdowns(
	ctx context.Context, where string, args []any, summary *InsightAggregateSummary,
) (err error) {
	//nolint:gosec // where clause uses parameterized placeholders only
	rows, err := s.db.QueryContext(ctx, insightBreakdownSQL+where, args...)
	if err != nil {
		return err
	}
	defer func() {
		// Named return, so this actually reaches the caller — but never at the
		// cost of a scan/iteration error, which is the more informative one.
		if cerr := rows.Close(); cerr != nil && err == nil {
			err = cerr
		}
	}()

	for rows.Next() {
		var tool, skill, plugin, mcpServer, mcpTool, effort, agent string
		if scanErr := rows.Scan(&tool, &skill, &plugin, &mcpServer, &mcpTool, &effort, &agent); scanErr != nil {
			return scanErr
		}
		summary.ToolBreakdowns = appendIfPresent(summary.ToolBreakdowns, tool)
		summary.SkillBreakdowns = appendIfPresent(summary.SkillBreakdowns, skill)
		summary.PluginBreakdowns = appendIfPresent(summary.PluginBreakdowns, plugin)
		summary.McpServerBreakdowns = appendIfPresent(summary.McpServerBreakdowns, mcpServer)
		summary.McpToolBreakdowns = appendIfPresent(summary.McpToolBreakdowns, mcpTool)
		summary.EffortBreakdowns = appendIfPresent(summary.EffortBreakdowns, effort)
		summary.AgentBreakdowns = appendIfPresent(summary.AgentBreakdowns, agent)
	}
	return rows.Err()
}

// appendIfPresent appends v unless it carries no breakdown data. An empty
// string and "{}" are both "nothing attributed" — the columns default to '{}'.
func appendIfPresent(dst []string, v string) []string {
	if v == "" || v == "{}" {
		return dst
	}
	return append(dst, v)
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
       total_duration_ms, active_duration_ms, claude_working_time_ms,
       cache_hit_rate, tokens_per_turn_avg, cost_estimate_usd,
       tool_error_count, has_errors,
       max_consecutive_tool_calls, longest_autonomous_chain,
       avg_user_response_time_ms, avg_claude_response_time_ms,
       session_type,
       skill_breakdown, plugin_breakdown, mcp_server_breakdown,
       mcp_tool_breakdown, effort_breakdown, unattributed_calls,
       agent_breakdown
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
		&r.ActiveDurationMs,
		&r.ClaudeWorkingTimeMs,
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
		&b.agents,
	)
	if err != nil {
		return nil, err
	}

	r.HasErrors = hasErrors != 0
	if t, parseErr := time.Parse(time.RFC3339, scannedAt); parseErr == nil {
		r.ScannedAt = t
	}
	r.ToolBreakdown = decodeToolBreakdown(toolBreakdown)
	b.decodeInto(&r)

	return &r, nil
}

// decodeToolBreakdown decodes the tool_breakdown column. Malformed JSON yields
// an empty map rather than an error: a breakdown is a derived convenience, and
// losing the whole insight row over one bad blob would be worse.
func decodeToolBreakdown(raw string) map[string]int {
	counts := make(map[string]int)
	if raw == "" || raw == "{}" {
		return counts
	}
	if err := json.Unmarshal([]byte(raw), &counts); err != nil {
		return make(map[string]int)
	}
	return counts
}

// decodeInto decodes every attribution column onto the record. Keeping this
// beside the struct means a newly added column has exactly two places to touch
// -- the scan list and here -- instead of being easy to scan and forget to
// decode.
func (b attributionColumns) decodeInto(r *InsightRecord) {
	r.SkillBreakdown = unmarshalCounts(b.skills)
	r.PluginBreakdown = unmarshalCounts(b.plugins)
	r.McpServerBreakdown = unmarshalCounts(b.mcpServers)
	r.McpToolBreakdown = unmarshalCounts(b.mcpTools)
	r.EffortBreakdown = unmarshalCounts(b.efforts)
	r.AgentBreakdown = unmarshalCounts(b.agents)
}

// attributionColumns holds the raw JSON of the attribution breakdown columns
// between Scan and decoding, keeping the scan argument list manageable.
type attributionColumns struct {
	skills     string
	plugins    string
	mcpServers string
	mcpTools   string
	efforts    string
	agents     string
}
