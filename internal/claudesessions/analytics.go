package claudesessions

import (
	"math"
	"sort"
	"strings"
	"time"
)

// ─── Pricing ──────────────────────────────────────────────────────────────────

type modelPricing struct {
	InputPerMTok  float64
	OutputPerMTok float64
	// Prompt-cache writes are billed by time-to-live: 1.25× input for the
	// 5-minute tier, 2× input for the 1-hour tier. Claude Code writes almost
	// exclusively 1-hour cache, so charging everything at the 5m rate — as this
	// table did until #180 — understates cache cost by ~37%.
	CacheWrite5mPerMTok float64
	CacheWrite1hPerMTok float64
	CacheReadPerMTok    float64 // 0.1× input
}

// syntheticModel is the placeholder Claude Code records for locally generated
// events that never hit the API. It is billed at zero and excluded from the
// model breakdown rather than being priced as a real model.
const syntheticModel = "<synthetic>"

// pricingTable maps a model-family key to its USD per-million-token rates.
// Rates checked against the published Anthropic pricing on 2026-08-08:
//
//	family  input  output  cache-write 5m (1.25×)  cache-write 1h (2×)  cache-read (0.1×)
//	fable   10.00   50.00                  12.50                20.00               1.00
//	opus     5.00   25.00                   6.25                10.00               0.50
//	sonnet   3.00   15.00                   3.75                 6.00               0.30
//	haiku    1.00    5.00                   1.25                 2.00               0.10
//
// Sonnet 5 carries a promotional $2/$10 rate through 2026-08-31; the list rate
// is used here because these figures are a cost estimate, not a bill, and a
// date-dependent table would silently change historical numbers.
var pricingTable = map[string]modelPricing{
	"fable":  {10.00, 50.00, 12.50, 20.00, 1.00},
	"opus":   {5.00, 25.00, 6.25, 10.00, 0.50},
	"sonnet": {3.00, 15.00, 3.75, 6.00, 0.30},
	"haiku":  {1.00, 5.00, 1.25, 2.00, 0.10},
}

// pricingForModel resolves the pricing for a model string such as
// "claude-opus-4-8". The second return value is false when the model has no
// known rates — a non-Anthropic model routed through Claude Code (`k3`,
// `glm-5.2`), an embedding model, or `<synthetic>`. Unknown models contribute
// no cost rather than being silently billed at another family's rates.
//
// Matching is deliberately anchored rather than a substring scan: a bare family
// name ("opus") and the "claude-<family>-..." form both resolve, but an
// arbitrary string that merely contains "opus" does not.
func pricingForModel(model string) (modelPricing, bool) {
	lower := strings.ToLower(strings.TrimSpace(model))
	if lower == "" || lower == syntheticModel {
		return modelPricing{}, false
	}
	for family, p := range pricingTable {
		if lower == family || strings.HasPrefix(lower, "claude-"+family+"-") {
			return p, true
		}
	}
	return modelPricing{}, false
}

// costForUsage prices one session's token usage. Returns false for a model with
// no known rates, so callers can account those tokens separately instead of
// folding an invented cost into the total.
func costForUsage(model string, u TokenUsage) (CostSummary, bool) {
	p, ok := pricingForModel(model)
	if !ok {
		return CostSummary{}, false
	}
	c := CostSummary{
		InputCostUSD:     float64(u.InputTokens) / 1_000_000 * p.InputPerMTok,
		OutputCostUSD:    float64(u.OutputTokens) / 1_000_000 * p.OutputPerMTok,
		CacheReadCostUSD: float64(u.CacheReadTokens) / 1_000_000 * p.CacheReadPerMTok,
		CacheWriteCostUSD: float64(u.CacheCreation5mTokens)/1_000_000*p.CacheWrite5mPerMTok +
			float64(u.CacheCreation1hTokens)/1_000_000*p.CacheWrite1hPerMTok,
	}
	c.TotalCostUSD = c.InputCostUSD + c.OutputCostUSD + c.CacheReadCostUSD + c.CacheWriteCostUSD
	return c, true
}

// unknownPricingAccumulator tallies the tokens and model names that carry no
// published rates, so the reported cost can state what it left out.
type unknownPricingAccumulator struct {
	seen   map[string]struct{}
	tokens int
}

func (a *unknownPricingAccumulator) add(model string, u TokenUsage) {
	if a.seen == nil {
		a.seen = map[string]struct{}{}
	}
	a.seen[model] = struct{}{}
	a.tokens += u.InputTokens + u.OutputTokens
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

// accumulateCost adds one session's cost to the running total, routing sessions
// on unpriced models to the unknown accumulator instead.
func accumulateCost(
	cost *CostSummary, unknown *unknownPricingAccumulator, model, label string, u TokenUsage,
) {
	c, priced := costForUsage(model, u)
	if !priced {
		unknown.add(label, u)
		return
	}
	cost.InputCostUSD += c.InputCostUSD
	cost.OutputCostUSD += c.OutputCostUSD
	cost.CacheReadCostUSD += c.CacheReadCostUSD
	cost.CacheWriteCostUSD += c.CacheWriteCostUSD
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
		accumulateCost(&cost, &unknown, s.Model, m, u)
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

func buildTimeSeries(sessions []ClaudeSessionSummary, from, to time.Time, granularity string) []TimeSeriesPoint {
	buckets := make(map[string]*TimeSeriesPoint)

	for _, s := range sessions {
		key := bucketKey(s.LastActivity, granularity)
		if buckets[key] == nil {
			buckets[key] = &TimeSeriesPoint{Date: bucketLabel(s.LastActivity, granularity)}
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

func buildMostActiveDays(sessions []ClaudeSessionSummary) []DayActivity {
	byDay := make(map[string]*DayActivity)
	for _, s := range sessions {
		key := s.LastActivity.Format("2006-01-02")
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

func buildHeatmap(sessions []ClaudeSessionSummary) []HeatmapCell {
	type cellKey struct{ dow, hour int }
	cells := make(map[cellKey]*HeatmapCell)
	for _, s := range sessions {
		k := cellKey{int(s.LastActivity.Weekday()), s.LastActivity.Hour()}
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

func buildHourlyActivity(sessions []ClaudeSessionSummary) []HourlyActivity {
	var hours [24]HourlyActivity
	for i := range hours {
		hours[i] = HourlyActivity{Hour: i}
	}
	for _, s := range sessions {
		h := s.LastActivity.Hour()
		u := s.TotalUsage()
		hours[h].Sessions++
		hours[h].Tokens += u.InputTokens + u.OutputTokens
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
		if c, priced := costForUsage(s.Model, s.TotalUsage()); priced {
			buckets[key].EstimatedCostUSD += c.TotalCostUSD
		}
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
