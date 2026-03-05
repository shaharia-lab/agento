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

	// Aggregate tool breakdowns from the per-session JSON strings.
	toolTotals := make(map[string]int)
	for _, tbJSON := range raw.ToolBreakdowns {
		var breakdown map[string]int
		if jsonErr := json.Unmarshal([]byte(tbJSON), &breakdown); jsonErr == nil {
			for tool, count := range breakdown {
				toolTotals[tool] += count
			}
		}
	}

	return &claudesessions.InsightAggregateSummary{
		TotalSessions:        raw.TotalSessions,
		AvgAutonomyScore:     raw.AvgAutonomyScore,
		AvgTurnCount:         raw.AvgTurnCount,
		AvgToolCallsTotal:    raw.AvgToolCallsTotal,
		TotalCostEstimateUSD: raw.TotalCostEstimateUSD,
		AvgCacheHitRate:      raw.AvgCacheHitRate,
		AvgTotalDurationMs:   raw.AvgTotalDurationMs,
		SessionsWithErrors:   raw.SessionsWithErrors,
		TopToolTotals:        toolTotals,
	}, nil
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
			SessionID: r.SessionID,
			FilePath:  r.FilePath,
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
		ProcessorVersion:        ins.ProcessorVersion,
		ScannedAt:               ins.ScannedAt,
		TurnCount:               ins.TurnCount,
		StepsPerTurnAvg:         ins.StepsPerTurnAvg,
		AutonomyScore:           ins.AutonomyScore,
		ToolCallsTotal:          ins.ToolCallsTotal,
		ToolBreakdown:           breakdown,
		ToolErrorRate:           ins.ToolErrorRate,
		TotalDurationMs:         ins.TotalDurationMs,
		ThinkingTimeMs:          ins.ThinkingTimeMs,
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
		ProcessorVersion:        r.ProcessorVersion,
		ScannedAt:               r.ScannedAt,
		TurnCount:               r.TurnCount,
		StepsPerTurnAvg:         r.StepsPerTurnAvg,
		AutonomyScore:           r.AutonomyScore,
		ToolCallsTotal:          r.ToolCallsTotal,
		ToolBreakdown:           breakdown,
		ToolErrorRate:           r.ToolErrorRate,
		TotalDurationMs:         r.TotalDurationMs,
		ThinkingTimeMs:          r.ThinkingTimeMs,
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
