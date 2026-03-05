package api

import (
	"net/http"
	"strings"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/claudesessions"
)

// handleGetClaudeSessionInsights returns the computed insight record for a
// single Claude Code session.
//
//	GET /api/claude-sessions/{id}/insights
func (s *Server) handleGetClaudeSessionInsights(w http.ResponseWriter, r *http.Request) {
	sessionID := chi.URLParam(r, "id")
	if sessionID == "" {
		s.writeError(w, http.StatusBadRequest, "session ID is required")
		return
	}

	insight, err := s.insightStore.Get(r.Context(), sessionID)
	if err != nil {
		s.logger.Error("failed to get session insight", "session_id", sessionID, "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to retrieve insight")
		return
	}
	if insight == nil {
		s.writeError(w, http.StatusNotFound, "insight not found for session")
		return
	}

	s.writeJSON(w, http.StatusOK, insight)
}

// handleGetClaudeSessionInsightsSummary returns aggregated insight statistics
// across all sessions, optionally filtered by session IDs. Scalar aggregations
// are computed in SQL to avoid loading all insight rows into memory.
//
//	GET /api/claude-sessions/insights/summary
//
// Query params:
//
//	ids  comma-separated list of session IDs to include (empty = all sessions)
func (s *Server) handleGetClaudeSessionInsightsSummary(w http.ResponseWriter, r *http.Request) {
	var sessionIDs []string
	if raw := r.URL.Query().Get("ids"); raw != "" {
		for _, id := range strings.Split(raw, ",") {
			if trimmed := strings.TrimSpace(id); trimmed != "" {
				sessionIDs = append(sessionIDs, trimmed)
			}
		}
	}

	agg, err := s.insightStore.GetSummary(r.Context(), sessionIDs)
	if err != nil {
		s.logger.Error("failed to get session insights summary", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to retrieve insights summary")
		return
	}

	s.writeJSON(w, http.StatusOK, buildInsightsSummaryFromAggregate(agg))
}

// insightsSummary holds aggregated statistics across multiple sessions.
type insightsSummary struct {
	TotalSessions        int         `json:"total_sessions"`
	AvgAutonomyScore     float64     `json:"avg_autonomy_score"`
	AvgTurnCount         float64     `json:"avg_turn_count"`
	AvgToolCallsTotal    float64     `json:"avg_tool_calls_total"`
	AvgCostEstimateUSD   float64     `json:"avg_cost_estimate_usd"`
	TotalCostEstimateUSD float64     `json:"total_cost_estimate_usd"`
	AvgCacheHitRate      float64     `json:"avg_cache_hit_rate"`
	SessionsWithErrors   int         `json:"sessions_with_errors"`
	AvgTotalDurationMs   float64     `json:"avg_total_duration_ms"`
	TopTools             []toolCount `json:"top_tools"`
}

// toolCount pairs a tool name with its aggregate call count.
type toolCount struct {
	Tool  string `json:"tool"`
	Count int    `json:"count"`
}

// buildInsightsSummaryFromAggregate converts SQL-computed aggregate stats into
// the HTTP response type.
func buildInsightsSummaryFromAggregate(agg *claudesessions.InsightAggregateSummary) *insightsSummary {
	if agg == nil || agg.TotalSessions == 0 {
		return &insightsSummary{TopTools: []toolCount{}}
	}
	n := float64(agg.TotalSessions)
	return &insightsSummary{
		TotalSessions:        agg.TotalSessions,
		AvgAutonomyScore:     agg.AvgAutonomyScore,
		AvgTurnCount:         agg.AvgTurnCount,
		AvgToolCallsTotal:    agg.AvgToolCallsTotal,
		TotalCostEstimateUSD: agg.TotalCostEstimateUSD,
		AvgCostEstimateUSD:   agg.TotalCostEstimateUSD / n,
		AvgCacheHitRate:      agg.AvgCacheHitRate,
		SessionsWithErrors:   agg.SessionsWithErrors,
		AvgTotalDurationMs:   agg.AvgTotalDurationMs,
		TopTools:             sortedToolCounts(agg.TopToolTotals, 10),
	}
}

// sortedToolCounts returns at most limit toolCount entries sorted by count descending.
func sortedToolCounts(totals map[string]int, limit int) []toolCount {
	counts := make([]toolCount, 0, len(totals))
	for tool, count := range totals {
		counts = append(counts, toolCount{Tool: tool, Count: count})
	}
	// Insertion sort (tool lists are small).
	for i := 1; i < len(counts); i++ {
		for j := i; j > 0 && counts[j].Count > counts[j-1].Count; j-- {
			counts[j], counts[j-1] = counts[j-1], counts[j]
		}
	}
	if limit > 0 && len(counts) > limit {
		counts = counts[:limit]
	}
	return counts
}
