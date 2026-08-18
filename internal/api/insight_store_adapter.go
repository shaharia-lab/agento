package api

import (
	"context"
	"encoding/json"

	"github.com/shaharia-lab/agento/internal/claudesessions"
	"github.com/shaharia-lab/agento/internal/storage"
)

// insightStoreAdapter bridges storage.SQLiteSessionInsightsStore (which uses
// storage.InsightRecord to avoid circular imports) with the
// claudesessions.InsightStorer interface (which uses claudesessions.SessionInsight).
type insightStoreAdapter struct {
	store *storage.SQLiteSessionInsightsStore
}

// NewInsightStoreAdapter wraps a SQLiteSessionInsightsStore so it satisfies
// claudesessions.InsightStorer.
func NewInsightStoreAdapter(store *storage.SQLiteSessionInsightsStore) claudesessions.InsightStorer {
	return &insightStoreAdapter{store: store}
}

func (a *insightStoreAdapter) Upsert(ctx context.Context, ins *claudesessions.SessionInsight) error {
	return a.store.Upsert(ctx, toInsightRecord(ins))
}

func (a *insightStoreAdapter) Get(ctx context.Context, sessionID string) (*claudesessions.SessionInsight, error) {
	r, err := a.store.Get(ctx, sessionID)
	if err != nil || r == nil {
		return nil, err
	}
	return fromInsightRecord(r), nil
}

func (a *insightStoreAdapter) GetMany(
	ctx context.Context, sessionIDs []string,
) ([]*claudesessions.SessionInsight, error) {
	if len(sessionIDs) == 0 {
		return nil, nil
	}
	records, err := a.store.GetMany(ctx, sessionIDs)
	if err != nil {
		return nil, err
	}
	results := make([]*claudesessions.SessionInsight, len(records))
	for i, r := range records {
		results[i] = fromInsightRecord(r)
	}
	return results, nil
}

func (a *insightStoreAdapter) GetSummary(
	ctx context.Context, sessionIDs []string,
) (*claudesessions.InsightAggregateSummary, error) {
	raw, err := a.store.GetAggregateSummary(ctx, sessionIDs)
	if err != nil {
		return nil, err
	}

	return &claudesessions.InsightAggregateSummary{
		TotalSessions:        raw.TotalSessions,
		AvgAutonomyScore:     raw.AvgAutonomyScore,
		AvgTurnCount:         raw.AvgTurnCount,
		AvgToolCallsTotal:    raw.AvgToolCallsTotal,
		TotalCostEstimateUSD: raw.TotalCostEstimateUSD,
		AvgCacheHitRate:      raw.AvgCacheHitRate,
		AvgTotalDurationMs:   raw.AvgTotalDurationMs,
		AvgActiveDurationMs:  raw.AvgActiveDurationMs,
		SessionsWithErrors:   raw.SessionsWithErrors,
		TotalToolErrors:      raw.TotalToolErrors,
		TopToolTotals:        mergeBreakdowns(raw.ToolBreakdowns),
		TopSkillTotals:       mergeBreakdowns(raw.SkillBreakdowns),
		TopPluginTotals:      mergeBreakdowns(raw.PluginBreakdowns),
		TopMcpServerTotals:   mergeBreakdowns(raw.McpServerBreakdowns),
		TopMcpToolTotals:     mergeBreakdowns(raw.McpToolBreakdowns),
		TopEffortTotals:      mergeBreakdowns(raw.EffortBreakdowns),
		TopAgentTotals:       mergeBreakdowns(raw.AgentBreakdowns),
		TotalToolCalls:       raw.TotalToolCalls,
		UnattributedCalls:    raw.UnattributedCalls,
	}, nil
}

// mergeBreakdowns sums the per-session breakdown JSON blobs into one total.
// A blob that fails to parse is skipped rather than failing the summary — one
// bad row should not blank the whole insights page.
func mergeBreakdowns(blobs []string) map[string]int {
	totals := make(map[string]int)
	for _, blob := range blobs {
		var breakdown map[string]int
		if jsonErr := json.Unmarshal([]byte(blob), &breakdown); jsonErr != nil {
			continue
		}
		for key, count := range breakdown {
			totals[key] += count
		}
	}
	return totals
}

func (a *insightStoreAdapter) NeedsProcessing(
	ctx context.Context, version int,
) ([]claudesessions.SessionToProcess, error) {
	raw, err := a.store.NeedsProcessing(ctx, version)
	if err != nil {
		return nil, err
	}
	sessions := make([]claudesessions.SessionToProcess, len(raw))
	for i, r := range raw {
		sessions[i] = claudesessions.SessionToProcess{
			SessionID:   r.SessionID,
			ProjectPath: r.ProjectPath,
			FilePath:    r.FilePath,
		}
	}
	return sessions, nil
}

// toInsightRecord converts a domain SessionInsight to a storage InsightRecord.
func toInsightRecord(ins *claudesessions.SessionInsight) storage.InsightRecord {
	breakdown := make(map[string]int, len(ins.ToolBreakdown))
	for k, v := range ins.ToolBreakdown {
		breakdown[k] = v
	}
	return storage.InsightRecord{
		SessionID:               ins.SessionID,
		ProjectPath:             ins.ProjectPath,
		ProcessorVersion:        ins.ProcessorVersion,
		ScannedAt:               ins.ScannedAt,
		TurnCount:               ins.TurnCount,
		StepsPerTurnAvg:         ins.StepsPerTurnAvg,
		AutonomyScore:           ins.AutonomyScore,
		ToolCallsTotal:          ins.ToolCallsTotal,
		ToolBreakdown:           breakdown,
		SkillBreakdown:          copyCounts(ins.SkillBreakdown),
		PluginBreakdown:         copyCounts(ins.PluginBreakdown),
		McpServerBreakdown:      copyCounts(ins.McpServerBreakdown),
		McpToolBreakdown:        copyCounts(ins.McpToolBreakdown),
		EffortBreakdown:         copyCounts(ins.EffortBreakdown),
		AgentBreakdown:          copyCounts(ins.AgentBreakdown),
		UnattributedCalls:       ins.UnattributedCalls,
		ToolErrorRate:           ins.ToolErrorRate,
		TotalDurationMs:         ins.TotalDurationMs,
		ActiveDurationMs:        ins.ActiveDurationMs,
		ClaudeWorkingTimeMs:     ins.ClaudeWorkingTimeMs,
		CacheHitRate:            ins.CacheHitRate,
		TokensPerTurnAvg:        ins.TokensPerTurnAvg,
		CostEstimateUSD:         ins.CostEstimateUSD,
		ToolErrorCount:          ins.ToolErrorCount,
		HasErrors:               ins.HasErrors,
		MaxConsecutiveToolCalls: ins.MaxConsecutiveToolCalls,
		LongestAutonomousChain:  ins.LongestAutonomousChain,
		AvgUserResponseTimeMs:   ins.AvgUserResponseTimeMs,
		AvgClaudeResponseTimeMs: ins.AvgClaudeResponseTimeMs,
		SessionType:             ins.SessionType,
	}
}

// fromInsightRecord converts a storage InsightRecord to a domain SessionInsight.
func fromInsightRecord(r *storage.InsightRecord) *claudesessions.SessionInsight {
	breakdown := make(map[string]int, len(r.ToolBreakdown))
	for k, v := range r.ToolBreakdown {
		breakdown[k] = v
	}
	return &claudesessions.SessionInsight{
		SessionID:               r.SessionID,
		ProjectPath:             r.ProjectPath,
		ProcessorVersion:        r.ProcessorVersion,
		ScannedAt:               r.ScannedAt,
		TurnCount:               r.TurnCount,
		StepsPerTurnAvg:         r.StepsPerTurnAvg,
		AutonomyScore:           r.AutonomyScore,
		ToolCallsTotal:          r.ToolCallsTotal,
		ToolBreakdown:           breakdown,
		SkillBreakdown:          copyCounts(r.SkillBreakdown),
		PluginBreakdown:         copyCounts(r.PluginBreakdown),
		McpServerBreakdown:      copyCounts(r.McpServerBreakdown),
		McpToolBreakdown:        copyCounts(r.McpToolBreakdown),
		EffortBreakdown:         copyCounts(r.EffortBreakdown),
		AgentBreakdown:          copyCounts(r.AgentBreakdown),
		UnattributedCalls:       r.UnattributedCalls,
		ToolErrorRate:           r.ToolErrorRate,
		TotalDurationMs:         r.TotalDurationMs,
		ActiveDurationMs:        r.ActiveDurationMs,
		ClaudeWorkingTimeMs:     r.ClaudeWorkingTimeMs,
		CacheHitRate:            r.CacheHitRate,
		TokensPerTurnAvg:        r.TokensPerTurnAvg,
		CostEstimateUSD:         r.CostEstimateUSD,
		ToolErrorCount:          r.ToolErrorCount,
		HasErrors:               r.HasErrors,
		MaxConsecutiveToolCalls: r.MaxConsecutiveToolCalls,
		LongestAutonomousChain:  r.LongestAutonomousChain,
		AvgUserResponseTimeMs:   r.AvgUserResponseTimeMs,
		AvgClaudeResponseTimeMs: r.AvgClaudeResponseTimeMs,
		SessionType:             r.SessionType,
	}
}

// copyCounts returns a defensive copy of a breakdown map, always non-nil so the
// JSON encoding is {} rather than null.
func copyCounts(src map[string]int) map[string]int {
	out := make(map[string]int, len(src))
	for k, v := range src {
		out[k] = v
	}
	return out
}
