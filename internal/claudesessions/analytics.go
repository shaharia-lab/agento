package claudesessions

import (
	"math"
	"sort"
	"strings"
	"time"
)

// ─── Pricing ──────────────────────────────────────────────────────────────────

type modelPricing struct {
	InputPerMTok      float64
	OutputPerMTok     float64
	CacheWritePerMTok float64
	CacheReadPerMTok  float64
}

// pricingTable maps a lowercase tier keyword to its USD per-million-token rates.
// Source: Anthropic pricing page (February 2025).
var pricingTable = map[string]modelPricing{
	"opus":   {15.00, 75.00, 18.75, 1.50},
	"sonnet": {3.00, 15.00, 3.75, 0.30},
	"haiku":  {0.80, 4.00, 1.00, 0.08},
}

var defaultPricing = pricingTable["sonnet"]

// pricingForModel resolves the pricing tier from a model string like
// "claude-sonnet-4-6". Falls back to sonnet pricing for unknown models.
func pricingForModel(model string) modelPricing {
	lower := strings.ToLower(model)
	for key, p := range pricingTable {
		if strings.Contains(lower, key) {
			return p
		}
	}
	return defaultPricing
}

// ─── Output types ─────────────────────────────────────────────────────────────

// AnalyticsReport is the complete response payload for GET /api/claude-analytics.
type AnalyticsReport struct {
	Summary          AnalyticsSummary       `json:"summary"`
	TimeSeries       []TimeSeriesPoint      `json:"time_series"`
	CacheEfficiency  []CacheEfficiencyPoint `json:"cache_efficiency"`
	ModelBreakdown   []ModelStat            `json:"model_breakdown"`
	SessionsPerModel []ModelSessionStat     `json:"sessions_per_model"`
	MostActiveDays   []DayActivity          `json:"most_active_days"`
	Heatmap          []HeatmapCell          `json:"heatmap"`
	HourlyActivity   []HourlyActivity       `json:"hourly_activity"`
	CostOverTime     []CostPoint            `json:"cost_over_time"`
	CostSummary      CostSummary            `json:"cost_summary"`
	Projects         []string               `json:"projects"`
}

// AnalyticsSummary holds the top-level KPI values.
type AnalyticsSummary struct {
	TotalSessions            int     `json:"total_sessions"`
	TotalTokens              int     `json:"total_tokens"`
	TotalInputTokens         int     `json:"total_input_tokens"`
	TotalOutputTokens        int     `json:"total_output_tokens"`
	TotalCacheReadTokens     int     `json:"total_cache_read_tokens"`
	TotalCacheCreationTokens int     `json:"total_cache_creation_tokens"`
	MostUsedModel            string  `json:"most_used_model"`
	AvgTokensPerSession      float64 `json:"avg_tokens_per_session"`
	EstimatedCostUSD         float64 `json:"estimated_cost_usd"`
}

// TimeSeriesPoint is one time bucket in the token usage over time chart.
type TimeSeriesPoint struct {
	Date             string `json:"date"`
	InputTokens      int    `json:"input_tokens"`
	OutputTokens     int    `json:"output_tokens"`
	CacheReadTokens  int    `json:"cache_read_tokens"`
	CacheWriteTokens int    `json:"cache_creation_tokens"`
	TotalTokens      int    `json:"total_tokens"`
	Sessions         int    `json:"sessions"`
}

// CacheEfficiencyPoint holds per-bucket cache hit rate data.
type CacheEfficiencyPoint struct {
	Date             string  `json:"date"`
	CacheHitRate     float64 `json:"cache_hit_rate"` // 0–100 %
	CachedTokens     int     `json:"cached_tokens"`
	TotalInputTokens int     `json:"total_input_tokens"`
}

// ModelStat describes token distribution across models.
type ModelStat struct {
	Model      string  `json:"model"`
	Tokens     int     `json:"tokens"`
	Percentage float64 `json:"percentage"`
}

// ModelSessionStat describes session count per model.
type ModelSessionStat struct {
	Model    string `json:"model"`
	Sessions int    `json:"sessions"`
}

// DayActivity holds aggregated activity for a single calendar day.
type DayActivity struct {
	Date     string `json:"date"`
	Sessions int    `json:"sessions"`
	Tokens   int    `json:"tokens"`
}

// HeatmapCell is one cell of the day-of-week × hour-of-day activity grid.
type HeatmapCell struct {
	DayOfWeek int `json:"day_of_week"` // 0=Sunday … 6=Saturday
	Hour      int `json:"hour"`        // 0–23
	Sessions  int `json:"sessions"`
	Tokens    int `json:"tokens"`
}

// HourlyActivity aggregates activity for each hour of the day (0–23).
type HourlyActivity struct {
	Hour     int `json:"hour"`
	Sessions int `json:"sessions"`
	Tokens   int `json:"tokens"`
}

// CostPoint holds estimated USD cost for a single time bucket.
type CostPoint struct {
	Date             string  `json:"date"`
	EstimatedCostUSD float64 `json:"estimated_cost_usd"`
}

// CostSummary breaks down total cost by token category.
type CostSummary struct {
	InputCostUSD      float64 `json:"input_cost_usd"`
	OutputCostUSD     float64 `json:"output_cost_usd"`
	CacheReadCostUSD  float64 `json:"cache_read_cost_usd"`
	CacheWriteCostUSD float64 `json:"cache_write_cost_usd"`
	TotalCostUSD      float64 `json:"total_cost_usd"`
}

// ─── AnalyticsParams ──────────────────────────────────────────────────────────

// AnalyticsParams controls filtering and granularity for an analytics request.
type AnalyticsParams struct {
	From    time.Time
	To      time.Time
	Project string // empty = all projects
}

// Granularity returns "hourly" when the range is ≤7 days, "daily" otherwise.
func (p AnalyticsParams) Granularity() string {
	if p.To.Sub(p.From) <= 7*24*time.Hour {
		return "hourly"
	}
	return "daily"
}

// ─── AggregateAnalytics ───────────────────────────────────────────────────────

// AggregateAnalytics builds an AnalyticsReport from a slice of session summaries.
// Filtering, bucketing, and aggregation all happen in memory — no disk I/O.
func AggregateAnalytics(sessions []ClaudeSessionSummary, p AnalyticsParams) AnalyticsReport {
	// Collect all distinct project paths before filtering.
	projectSet := make(map[string]struct{})
	for _, s := range sessions {
		projectSet[s.ProjectPath] = struct{}{}
	}
	projects := make([]string, 0, len(projectSet))
	for proj := range projectSet {
		projects = append(projects, proj)
	}
	sort.Strings(projects)

	filtered := filterSessions(sessions, p)

	if len(filtered) == 0 {
		return AnalyticsReport{
			TimeSeries:       []TimeSeriesPoint{},
			CacheEfficiency:  []CacheEfficiencyPoint{},
			ModelBreakdown:   []ModelStat{},
			SessionsPerModel: []ModelSessionStat{},
			MostActiveDays:   []DayActivity{},
			Heatmap:          []HeatmapCell{},
			HourlyActivity:   buildHourlyActivity(nil),
			CostOverTime:     []CostPoint{},
			Projects:         projects,
		}
	}

	granularity := p.Granularity()
	summary, costSummary := buildSummary(filtered)
	timeSeries := buildTimeSeries(filtered, p.From, p.To, granularity)

	return AnalyticsReport{
		Summary:          summary,
		TimeSeries:       timeSeries,
		CacheEfficiency:  buildCacheEfficiency(timeSeries),
		ModelBreakdown:   buildModelBreakdown(filtered),
		SessionsPerModel: buildSessionsPerModel(filtered),
		MostActiveDays:   buildMostActiveDays(filtered),
		Heatmap:          buildHeatmap(filtered),
		HourlyActivity:   buildHourlyActivity(filtered),
		CostOverTime:     buildCostOverTime(filtered, p.From, p.To, granularity),
		CostSummary:      costSummary,
		Projects:         projects,
	}
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

func filterSessions(sessions []ClaudeSessionSummary, p AnalyticsParams) []ClaudeSessionSummary {
	out := make([]ClaudeSessionSummary, 0, len(sessions))
	for _, s := range sessions {
		if s.LastActivity.Before(p.From) || s.LastActivity.After(p.To) {
			continue
		}
		if p.Project != "" && s.ProjectPath != p.Project {
			continue
		}
		out = append(out, s)
	}
	return out
}

func buildSummary(sessions []ClaudeSessionSummary) (AnalyticsSummary, CostSummary) {
	var totalInput, totalOutput, totalCacheRead, totalCacheWrite int
	modelCount := make(map[string]int)
	var cost CostSummary

	for _, s := range sessions {
		totalInput += s.Usage.InputTokens
		totalOutput += s.Usage.OutputTokens
		totalCacheRead += s.Usage.CacheReadTokens
		totalCacheWrite += s.Usage.CacheCreationTokens

		m := s.Model
		if m == "" {
			m = "unknown"
		}
		modelCount[m]++

		p := pricingForModel(s.Model)
		cost.InputCostUSD += float64(s.Usage.InputTokens) / 1_000_000 * p.InputPerMTok
		cost.OutputCostUSD += float64(s.Usage.OutputTokens) / 1_000_000 * p.OutputPerMTok
		cost.CacheReadCostUSD += float64(s.Usage.CacheReadTokens) / 1_000_000 * p.CacheReadPerMTok
		cost.CacheWriteCostUSD += float64(s.Usage.CacheCreationTokens) / 1_000_000 * p.CacheWritePerMTok
	}
	cost.TotalCostUSD = cost.InputCostUSD + cost.OutputCostUSD + cost.CacheReadCostUSD + cost.CacheWriteCostUSD

	mostUsed := ""
	maxCount := 0
	for m, c := range modelCount {
		if c > maxCount {
			maxCount = c
			mostUsed = m
		}
	}

	total := totalInput + totalOutput
	avg := 0.0
	if len(sessions) > 0 {
		avg = math.Round(float64(total)/float64(len(sessions))*10) / 10
	}

	return AnalyticsSummary{
		TotalSessions:            len(sessions),
		TotalTokens:              total,
		TotalInputTokens:         totalInput,
		TotalOutputTokens:        totalOutput,
		TotalCacheReadTokens:     totalCacheRead,
		TotalCacheCreationTokens: totalCacheWrite,
		MostUsedModel:            mostUsed,
		AvgTokensPerSession:      avg,
		EstimatedCostUSD:         cost.TotalCostUSD,
	}, cost
}

func buildTimeSeries(sessions []ClaudeSessionSummary, from, to time.Time, granularity string) []TimeSeriesPoint {
	buckets := make(map[string]*TimeSeriesPoint)

	for _, s := range sessions {
		key := bucketKey(s.LastActivity, granularity)
		if buckets[key] == nil {
			buckets[key] = &TimeSeriesPoint{Date: bucketLabel(s.LastActivity, granularity)}
		}
		b := buckets[key]
		b.InputTokens += s.Usage.InputTokens
		b.OutputTokens += s.Usage.OutputTokens
		b.CacheReadTokens += s.Usage.CacheReadTokens
		b.CacheWriteTokens += s.Usage.CacheCreationTokens
		b.TotalTokens += s.Usage.InputTokens + s.Usage.OutputTokens
		b.Sessions++
	}

	return fillTimeSeries(buckets, from, to, granularity)
}

func buildCacheEfficiency(ts []TimeSeriesPoint) []CacheEfficiencyPoint {
	out := make([]CacheEfficiencyPoint, 0, len(ts))
	for _, p := range ts {
		rate := 0.0
		denom := p.InputTokens + p.CacheReadTokens
		if denom > 0 {
			rate = math.Round(float64(p.CacheReadTokens)/float64(denom)*10000) / 100
		}
		out = append(out, CacheEfficiencyPoint{
			Date:             p.Date,
			CacheHitRate:     rate,
			CachedTokens:     p.CacheReadTokens,
			TotalInputTokens: p.InputTokens,
		})
	}
	return out
}

func buildModelBreakdown(sessions []ClaudeSessionSummary) []ModelStat {
	tokensByModel := make(map[string]int)
	total := 0
	for _, s := range sessions {
		m := s.Model
		if m == "" {
			m = "unknown"
		}
		t := s.Usage.InputTokens + s.Usage.OutputTokens
		tokensByModel[m] += t
		total += t
	}
	out := make([]ModelStat, 0, len(tokensByModel))
	for m, t := range tokensByModel {
		pct := 0.0
		if total > 0 {
			pct = math.Round(float64(t)/float64(total)*1000) / 10
		}
		out = append(out, ModelStat{Model: m, Tokens: t, Percentage: pct})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Tokens > out[j].Tokens })
	return out
}

func buildSessionsPerModel(sessions []ClaudeSessionSummary) []ModelSessionStat {
	countByModel := make(map[string]int)
	for _, s := range sessions {
		m := s.Model
		if m == "" {
			m = "unknown"
		}
		countByModel[m]++
	}
	out := make([]ModelSessionStat, 0, len(countByModel))
	for m, c := range countByModel {
		out = append(out, ModelSessionStat{Model: m, Sessions: c})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Sessions > out[j].Sessions })
	return out
}

func buildMostActiveDays(sessions []ClaudeSessionSummary) []DayActivity {
	byDay := make(map[string]*DayActivity)
	for _, s := range sessions {
		key := s.LastActivity.Format("2006-01-02")
		if byDay[key] == nil {
			byDay[key] = &DayActivity{Date: key}
		}
		byDay[key].Sessions++
		byDay[key].Tokens += s.Usage.InputTokens + s.Usage.OutputTokens
	}
	out := make([]DayActivity, 0, len(byDay))
	for _, d := range byDay {
		out = append(out, *d)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Tokens > out[j].Tokens })
	if len(out) > 30 {
		out = out[:30]
	}
	return out
}

func buildHeatmap(sessions []ClaudeSessionSummary) []HeatmapCell {
	type cellKey struct{ dow, hour int }
	cells := make(map[cellKey]*HeatmapCell)
	for _, s := range sessions {
		k := cellKey{int(s.LastActivity.Weekday()), s.LastActivity.Hour()}
		if cells[k] == nil {
			cells[k] = &HeatmapCell{DayOfWeek: k.dow, Hour: k.hour}
		}
		cells[k].Sessions++
		cells[k].Tokens += s.Usage.InputTokens + s.Usage.OutputTokens
	}
	out := make([]HeatmapCell, 0, len(cells))
	for _, c := range cells {
		out = append(out, *c)
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].DayOfWeek != out[j].DayOfWeek {
			return out[i].DayOfWeek < out[j].DayOfWeek
		}
		return out[i].Hour < out[j].Hour
	})
	return out
}

func buildHourlyActivity(sessions []ClaudeSessionSummary) []HourlyActivity {
	var hours [24]HourlyActivity
	for i := range hours {
		hours[i] = HourlyActivity{Hour: i}
	}
	for _, s := range sessions {
		h := s.LastActivity.Hour()
		hours[h].Sessions++
		hours[h].Tokens += s.Usage.InputTokens + s.Usage.OutputTokens
	}
	out := make([]HourlyActivity, 24)
	copy(out, hours[:])
	return out
}

func buildCostOverTime(sessions []ClaudeSessionSummary, from, to time.Time, granularity string) []CostPoint {
	buckets := make(map[string]*CostPoint)
	for _, s := range sessions {
		key := bucketKey(s.LastActivity, granularity)
		if buckets[key] == nil {
			buckets[key] = &CostPoint{Date: bucketLabel(s.LastActivity, granularity)}
		}
		p := pricingForModel(s.Model)
		buckets[key].EstimatedCostUSD +=
			float64(s.Usage.InputTokens)/1_000_000*p.InputPerMTok +
				float64(s.Usage.OutputTokens)/1_000_000*p.OutputPerMTok +
				float64(s.Usage.CacheReadTokens)/1_000_000*p.CacheReadPerMTok +
				float64(s.Usage.CacheCreationTokens)/1_000_000*p.CacheWritePerMTok
	}

	step := 24 * time.Hour
	if granularity == "hourly" {
		step = time.Hour
	}
	var result []CostPoint
	for cur := from; !cur.After(to); cur = cur.Add(step) {
		key := bucketKey(cur, granularity)
		if b, ok := buckets[key]; ok {
			result = append(result, *b)
		} else {
			result = append(result, CostPoint{Date: bucketLabel(cur, granularity)})
		}
	}
	return result
}

// ─── Time bucket helpers ──────────────────────────────────────────────────────

func bucketKey(t time.Time, granularity string) string {
	if granularity == "hourly" {
		return t.Format("2006-01-02T15")
	}
	return t.Format("2006-01-02")
}

func bucketLabel(t time.Time, granularity string) string {
	return bucketKey(t, granularity)
}

func fillTimeSeries(buckets map[string]*TimeSeriesPoint, from, to time.Time, granularity string) []TimeSeriesPoint {
	step := 24 * time.Hour
	if granularity == "hourly" {
		step = time.Hour
	}
	var result []TimeSeriesPoint
	for cur := from; !cur.After(to); cur = cur.Add(step) {
		key := bucketKey(cur, granularity)
		if b, ok := buckets[key]; ok {
			result = append(result, *b)
		} else {
			result = append(result, TimeSeriesPoint{Date: bucketLabel(cur, granularity)})
		}
	}
	return result
}
