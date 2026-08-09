package claudesessions

import (
	"math"
	"sort"
	"time"
)

// ─── Pricing ──────────────────────────────────────────────────────────────────
//
// Rates live in the persisted pricing catalog (internal/pricing), resolved per
// model and per message timestamp, so historical cost keeps the rate that was
// in force when the tokens were spent.
//
// Nothing here prices anything. Since #188 the scan stores each session's cost
// on its cache row, and these aggregates read it — a session summary carries no
// per-message timing, so re-deriving cost here could only approximate it by
// picking one model and one instant for the whole session. That approximation
// was the second, divergent cost path; reading the stored value is what makes
// the session list and these totals agree by construction.

// syntheticModel is the placeholder Claude Code records for locally generated
// events that never hit the API. It is billed at zero and excluded from the
// model breakdown rather than being priced as a real model.
const syntheticModel = "<synthetic>"

// unknownPricingAccumulator tallies the tokens and model names that carry no
// published rates, so the reported cost can state what it left out.
type unknownPricingAccumulator struct {
	seen   map[string]struct{}
	tokens int
}

// addModel records an unpriced model. Token counts come from the session's
// own stored tally rather than being attributed here.
func (a *unknownPricingAccumulator) addModel(model string) {
	if a.seen == nil {
		a.seen = map[string]struct{}{}
	}
	a.seen[model] = struct{}{}
}

// models returns the distinct unpriced model identifiers, sorted.
func (a *unknownPricingAccumulator) models() []string {
	out := make([]string, 0, len(a.seen))
	for m := range a.seen {
		out = append(out, m)
	}
	sort.Strings(out)
	return out
}

// displayModel is the label used for a session whose model string may be empty.
func displayModel(model string) string {
	if model == "" {
		return "unknown"
	}
	return model
}

// addStoredCost folds one session's stored cost into the running total.
//
// The figure is read rather than re-derived: the scanner already priced every
// assistant message at its own model and timestamp, which aggregate re-pricing
// here could only approximate — it would have to pick one model and one instant
// for the whole session. Reading the stored value is what makes the session
// list, the detail page and this total the same number by construction.
//
// A session that used a model with no known rate reports it in UnpricedModels;
// those tokens are tallied separately so the total can state what it left out.
func addStoredCost(cost *CostSummary, unknown *unknownPricingAccumulator, s ClaudeSessionSummary) {
	c := s.TotalCost()
	cost.InputCostUSD += c.InputUSD
	cost.OutputCostUSD += c.OutputUSD
	cost.CacheReadCostUSD += c.CacheReadUSD
	cost.CacheWriteCostUSD += c.CacheWriteUSD

	for _, m := range s.UnpricedModels {
		unknown.addModel(m)
	}
	unknown.tokens += s.UnpricedTokens
}

// mostFrequent returns the key with the highest count, or "" when empty.
func mostFrequent(counts map[string]int) string {
	best, maxCount := "", 0
	for k, c := range counts {
		if c > maxCount {
			best, maxCount = k, c
		}
	}
	return best
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
	// UnknownPricingTokens counts tokens belonging to models with no published
	// rates (non-Anthropic models routed through Claude Code, `<synthetic>`).
	// They contribute nothing to EstimatedCostUSD; surfacing the count keeps
	// that omission visible rather than making the total look complete.
	UnknownPricingTokens int `json:"unknown_pricing_tokens"`
	// UnknownPricingModels lists those model identifiers, sorted.
	UnknownPricingModels []string `json:"unknown_pricing_models"`
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
	// Loc is the timezone the day, hour and weekday buckets are derived in.
	// Storage and transport stay UTC; only aggregation and labeling move, so
	// "when do I work?" is answered in the hours the user actually worked.
	// Nil means UTC, which keeps callers that predate this working unchanged.
	Loc *time.Location
}

// location returns the bucketing timezone, defaulting to UTC.
func (p AnalyticsParams) location() *time.Location {
	if p.Loc == nil {
		return time.UTC
	}
	return p.Loc
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

	loc := p.location()
	filtered := filterSessions(sessions, p)

	if len(filtered) == 0 {
		return AnalyticsReport{
			TimeSeries:       []TimeSeriesPoint{},
			CacheEfficiency:  []CacheEfficiencyPoint{},
			ModelBreakdown:   []ModelStat{},
			SessionsPerModel: []ModelSessionStat{},
			MostActiveDays:   []DayActivity{},
			Heatmap:          []HeatmapCell{},
			HourlyActivity:   buildHourlyActivity(nil, loc),
			CostOverTime:     []CostPoint{},
			Projects:         projects,
		}
	}

	granularity := p.Granularity()
	summary, costSummary := buildSummary(filtered)
	timeSeries := buildTimeSeries(filtered, p.From, p.To, granularity, loc)

	return AnalyticsReport{
		Summary:          summary,
		TimeSeries:       timeSeries,
		CacheEfficiency:  buildCacheEfficiency(timeSeries),
		ModelBreakdown:   buildModelBreakdown(filtered),
		SessionsPerModel: buildSessionsPerModel(filtered),
		MostActiveDays:   buildMostActiveDays(filtered, loc),
		Heatmap:          buildHeatmap(filtered, loc),
		HourlyActivity:   buildHourlyActivity(filtered, loc),
		CostOverTime:     buildCostOverTime(filtered, p.From, p.To, granularity, loc),
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

	var unknown unknownPricingAccumulator

	for _, s := range sessions {
		u := s.TotalUsage()
		totalInput += u.InputTokens
		totalOutput += u.OutputTokens
		totalCacheRead += u.CacheReadTokens
		totalCacheWrite += u.CacheCreationTokens

		m := displayModel(s.Model)
		if m != syntheticModel {
			// Kept out of MostUsedModel for the same reason it is kept out of
			// the model breakdowns — it is not a model anyone ran.
			modelCount[m]++
		}
		addStoredCost(&cost, &unknown, s)
	}
	cost.TotalCostUSD = cost.InputCostUSD + cost.OutputCostUSD + cost.CacheReadCostUSD + cost.CacheWriteCostUSD

	mostUsed := mostFrequent(modelCount)

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
		UnknownPricingTokens:     unknown.tokens,
		UnknownPricingModels:     unknown.models(),
	}, cost
}

func buildTimeSeries(
	sessions []ClaudeSessionSummary, from, to time.Time, granularity string, loc *time.Location,
) []TimeSeriesPoint {
	buckets := make(map[string]*TimeSeriesPoint)

	for _, s := range sessions {
		key := bucketKey(s.LastActivity, granularity, loc)
		if buckets[key] == nil {
			buckets[key] = &TimeSeriesPoint{Date: bucketLabel(s.LastActivity, granularity, loc)}
		}
		b := buckets[key]
		u := s.TotalUsage()
		b.InputTokens += u.InputTokens
		b.OutputTokens += u.OutputTokens
		b.CacheReadTokens += u.CacheReadTokens
		b.CacheWriteTokens += u.CacheCreationTokens
		b.TotalTokens += u.InputTokens + u.OutputTokens
		b.Sessions++
	}

	return fillTimeSeries(buckets, from, to, granularity, loc)
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
		if s.Model == syntheticModel {
			continue // locally generated, never billed — not a real model
		}
		m := s.Model
		if m == "" {
			m = "unknown"
		}
		u := s.TotalUsage()
		t := u.InputTokens + u.OutputTokens
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
		if s.Model == syntheticModel {
			continue // see buildModelBreakdown
		}
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

func buildMostActiveDays(sessions []ClaudeSessionSummary, loc *time.Location) []DayActivity {
	byDay := make(map[string]*DayActivity)
	for _, s := range sessions {
		// Via bucketKey so there is exactly one place that formats a day.
		key := bucketKey(s.LastActivity, "daily", loc)
		if byDay[key] == nil {
			byDay[key] = &DayActivity{Date: key}
		}
		u := s.TotalUsage()
		byDay[key].Sessions++
		byDay[key].Tokens += u.InputTokens + u.OutputTokens
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

func buildHeatmap(sessions []ClaudeSessionSummary, loc *time.Location) []HeatmapCell {
	type cellKey struct{ dow, hour int }
	cells := make(map[cellKey]*HeatmapCell)
	for _, s := range sessions {
		at := s.LastActivity.In(loc)
		k := cellKey{int(at.Weekday()), at.Hour()}
		if cells[k] == nil {
			cells[k] = &HeatmapCell{DayOfWeek: k.dow, Hour: k.hour}
		}
		u := s.TotalUsage()
		cells[k].Sessions++
		cells[k].Tokens += u.InputTokens + u.OutputTokens
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

func buildHourlyActivity(sessions []ClaudeSessionSummary, loc *time.Location) []HourlyActivity {
	var hours [24]HourlyActivity
	for i := range hours {
		hours[i] = HourlyActivity{Hour: i}
	}
	for _, s := range sessions {
		h := s.LastActivity.In(loc).Hour()
		u := s.TotalUsage()
		hours[h].Sessions++
		hours[h].Tokens += u.InputTokens + u.OutputTokens
	}
	out := make([]HourlyActivity, 24)
	copy(out, hours[:])
	return out
}

func buildCostOverTime(
	sessions []ClaudeSessionSummary, from, to time.Time, granularity string, loc *time.Location,
) []CostPoint {
	buckets := make(map[string]*CostPoint)
	for _, s := range sessions {
		key := bucketKey(s.LastActivity, granularity, loc)
		if buckets[key] == nil {
			buckets[key] = &CostPoint{Date: bucketLabel(s.LastActivity, granularity, loc)}
		}
		// Stored cost, for the same reason buildSummary reads it — the two must
		// add up to the same money.
		buckets[key].EstimatedCostUSD += s.TotalCost().TotalUSD
	}

	var result []CostPoint
	walkBuckets(from, to, granularity, loc, func(key string, cur time.Time) {
		if b, ok := buckets[key]; ok {
			result = append(result, *b)
			return
		}
		result = append(result, CostPoint{Date: bucketLabel(cur, granularity, loc)})
	})
	return result
}

// ─── Time bucket helpers ──────────────────────────────────────────────────────

// bucketKey derives a session's bucket in loc. Format renders in the time's own
// location, so the conversion has to happen here rather than at the edges — an
// instant is only a day or an hour once you say whose day you mean.
func bucketKey(t time.Time, granularity string, loc *time.Location) string {
	t = t.In(loc)
	if granularity == "hourly" {
		return t.Format("2006-01-02T15")
	}
	return t.Format("2006-01-02")
}

func bucketLabel(t time.Time, granularity string, loc *time.Location) string {
	return bucketKey(t, granularity, loc)
}

// walkBuckets calls fn once per bucket from `from` to `to` inclusive, stepping
// in loc.
//
// Daily steps advance the calendar day rather than adding 24 hours: across a
// DST transition a local day is 23 or 25 hours long, and a fixed 24h step drifts
// off the wall clock, duplicating one day key and skipping another.
func walkBuckets(from, to time.Time, granularity string, loc *time.Location, fn func(string, time.Time)) {
	cur := from.In(loc)
	end := to.In(loc)
	for !cur.After(end) {
		fn(bucketKey(cur, granularity, loc), cur)
		if granularity == "hourly" {
			cur = cur.Add(time.Hour)
			continue
		}
		cur = cur.AddDate(0, 0, 1)
	}
}

func fillTimeSeries(
	buckets map[string]*TimeSeriesPoint, from, to time.Time, granularity string, loc *time.Location,
) []TimeSeriesPoint {
	var result []TimeSeriesPoint
	walkBuckets(from, to, granularity, loc, func(key string, cur time.Time) {
		if b, ok := buckets[key]; ok {
			result = append(result, *b)
			return
		}
		result = append(result, TimeSeriesPoint{Date: bucketLabel(cur, granularity, loc)})
	})
	return result
}
