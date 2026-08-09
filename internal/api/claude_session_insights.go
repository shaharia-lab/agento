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
// across all sessions, optionally filtered by session IDs and/or date range.
// Scalar aggregations are computed in SQL to avoid loading all rows into memory.
//
//	GET /api/claude-sessions/insights/summary
//
// Query params:
//
//	ids   comma-separated list of session IDs to include (empty = all sessions)
//	from  inclusive start (YYYY-MM-DD or RFC3339); filters by last_activity
//	to    inclusive end   (YYYY-MM-DD or RFC3339); filters by last_activity
//	tz    IANA timezone a bare date's day boundaries are resolved in
func (s *Server) handleGetClaudeSessionInsightsSummary(w http.ResponseWriter, r *http.Request) {
	// The window, project and timezone are read by the same parser the analytics
	// endpoint uses, and applied by the same filter, so the two dashboards cover
	// one set of sessions for a given range instead of two overlapping ones.
	params := parseAnalyticsParams(r)
	windowed := claudesessions.FilterSessions(s.claudeSessionCache.List(), params)
	sessionIDs := claudesessions.SessionIDs(windowed)

	// An explicit ids list narrows the window rather than replacing it, so a
	// caller cannot accidentally widen the range by naming a session outside it.
	if explicit := parseSessionIDs(r.URL.Query().Get("ids")); len(explicit) > 0 {
		sessionIDs = intersectIDs(sessionIDs, explicit)
	}

	agg, err := s.insightStore.GetSummary(r.Context(), sessionIDs)
	if err != nil {
		s.logger.Error("failed to get session insights summary", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to retrieve insights summary")
		return
	}

	s.writeJSON(w, http.StatusOK, buildInsightsSummaryFromAggregate(agg))
}

// parseSessionIDs splits the comma-separated `ids` parameter, dropping blanks.
func parseSessionIDs(raw string) []string {
	if raw == "" {
		return nil
	}
	var ids []string
	for _, id := range strings.Split(raw, ",") {
		if trimmed := strings.TrimSpace(id); trimmed != "" {
			ids = append(ids, trimmed)
		}
	}
	return ids
}

// intersectIDs returns the members of base that also appear in filter,
// preserving base's order.
func intersectIDs(base, filter []string) []string {
	wanted := make(map[string]struct{}, len(filter))
	for _, id := range filter {
		wanted[id] = struct{}{}
	}
	out := make([]string, 0, len(base))
	for _, id := range base {
		if _, ok := wanted[id]; ok {
			out = append(out, id)
		}
	}
	return out
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
	// Attribution breakdowns. Each counts tool calls, so they are directly
	// comparable with TopTools.
	// TotalToolCalls and UnattributedCalls let the UI state what share of
	// tool calls the skill breakdown actually accounts for.
	TotalToolCalls    int         `json:"total_tool_calls"`
	UnattributedCalls int         `json:"unattributed_calls"`
	TopSkills         []toolCount `json:"top_skills"`
	TopPlugins        []toolCount `json:"top_plugins"`
	TopMcpServers     []toolCount `json:"top_mcp_servers"`
	// TopMcpTools is the drill-down under TopMcpServers, not a peer dimension.
	TopMcpTools []toolCount `json:"top_mcp_tools"`
	TopEfforts  []toolCount `json:"top_efforts"`
	TopAgents   []toolCount `json:"top_agents"`
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
		return &insightsSummary{
			TopTools:      []toolCount{},
			TopSkills:     []toolCount{},
			TopPlugins:    []toolCount{},
			TopMcpServers: []toolCount{},
			TopMcpTools:   []toolCount{},
			TopEfforts:    []toolCount{},
			TopAgents:     []toolCount{},
		}
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
		TopTools:             sortedToolCounts(agg.TopToolTotals),
		TotalToolCalls:       agg.TotalToolCalls,
		UnattributedCalls:    agg.UnattributedCalls,
		TopSkills:            sortedToolCounts(agg.TopSkillTotals),
		TopPlugins:           sortedToolCounts(agg.TopPluginTotals),
		TopMcpServers:        sortedToolCounts(agg.TopMcpServerTotals),
		TopMcpTools:          sortedToolCounts(agg.TopMcpToolTotals),
		TopEfforts:           sortedToolCounts(agg.TopEffortTotals),
		TopAgents:            sortedToolCounts(agg.TopAgentTotals),
	}
}

// topBreakdownEntries is how many entries each breakdown panel shows.
const topBreakdownEntries = 10

// sortedToolCounts returns the top entries sorted by count descending.
func sortedToolCounts(totals map[string]int) []toolCount {
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
	if len(counts) > topBreakdownEntries {
		counts = counts[:topBreakdownEntries]
	}
	return counts
}
