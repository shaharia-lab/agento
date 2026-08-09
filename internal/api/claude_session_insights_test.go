package api

import (
	"encoding/json"
	"testing"

	"github.com/shaharia-lab/agento/internal/claudesessions"
)

// TestBuildInsightsSummary_EmptyBreakdownsMarshalAsArrays pins the empty-summary
// literal. A breakdown field left out of it marshals as `null`, and the page
// reads `.length` on it — so a forgotten field is a blank insights page rather
// than an empty panel.
func TestBuildInsightsSummary_EmptyBreakdownsMarshalAsArrays(t *testing.T) {
	raw, err := json.Marshal(buildInsightsSummaryFromAggregate(nil))
	if err != nil {
		t.Fatal(err)
	}

	var decoded map[string]json.RawMessage
	if err := json.Unmarshal(raw, &decoded); err != nil {
		t.Fatal(err)
	}

	for _, key := range []string{
		"top_tools", "top_skills", "top_plugins", "top_mcp_servers",
		"top_mcp_tools", "top_efforts", "top_agents",
	} {
		got, ok := decoded[key]
		if !ok {
			t.Errorf("%s is missing from the response", key)
			continue
		}
		if string(got) != "[]" {
			t.Errorf("%s = %s, want [] — a null here blanks the insights page", key, got)
		}
	}
}

// TestBuildInsightsSummary_SurfacesEveryAttributionDimension covers the wiring
// #202 added: three dimensions were parsed and stored but never aggregated, so
// the failure mode is a silently absent panel, not an error.
func TestBuildInsightsSummary_SurfacesEveryAttributionDimension(t *testing.T) {
	agg := &claudesessions.InsightAggregateSummary{
		TotalSessions:      2,
		TopToolTotals:      map[string]int{"Bash": 9},
		TopSkillTotals:     map[string]int{"vibexp:prime": 4},
		TopPluginTotals:    map[string]int{"lab-workflow": 4},
		TopMcpServerTotals: map[string]int{"vibexp_io_vibexp_team": 3},
		TopMcpToolTotals:   map[string]int{"vibexp_io_post_to_feed": 2},
		TopEffortTotals:    map[string]int{"high": 8, "low": 1},
		TopAgentTotals:     map[string]int{"Explore": 5, "general-purpose": 1},
	}

	got := buildInsightsSummaryFromAggregate(agg)

	for _, tc := range []struct {
		name  string
		got   []toolCount
		want  string
		count int
	}{
		{"top_mcp_tools", got.TopMcpTools, "vibexp_io_post_to_feed", 2},
		{"top_efforts", got.TopEfforts, "high", 8},
		{"top_agents", got.TopAgents, "Explore", 5},
	} {
		if len(tc.got) == 0 {
			t.Errorf("%s is empty — the dimension is stored but not aggregated", tc.name)
			continue
		}
		// sortedToolCounts orders by count descending, so the busiest leads.
		if tc.got[0].Tool != tc.want || tc.got[0].Count != tc.count {
			t.Errorf("%s[0] = %+v, want {%s %d}", tc.name, tc.got[0], tc.want, tc.count)
		}
	}

	// Each dimension must stay distinct rather than being wired to a neighbor.
	if got.TopMcpTools[0].Tool == got.TopMcpServers[0].Tool {
		t.Error("top_mcp_tools and top_mcp_servers are cross-wired")
	}
	if len(got.TopAgents) != 2 {
		t.Errorf("top_agents has %d entries, want 2", len(got.TopAgents))
	}
}
