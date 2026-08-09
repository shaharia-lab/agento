package claudesessions

import (
	"math"
	"sort"
	"strings"
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
	Summary             AnalyticsSummary       `json:"summary"`
	TimeSeries          []TimeSeriesPoint      `json:"time_series"`
	CacheEfficiency     []CacheEfficiencyPoint `json:"cache_efficiency"`
	ModelBreakdown      []ModelStat            `json:"model_breakdown"`
	SessionsPerModel    []ModelSessionStat     `json:"sessions_per_model"`
	CostByModel         []ModelCostStat        `json:"cost_by_model"`
	CostOverTimeByModel []StackedCostPoint     `json:"cost_over_time_by_model"`
	MostActiveDays      []DayActivity          `json:"most_active_days"`
	Heatmap             []HeatmapCell          `json:"heatmap"`
	HourlyActivity      []HourlyActivity       `json:"hourly_activity"`
	CostOverTime        []CostPoint            `json:"cost_over_time"`
	CostSummary         CostSummary            `json:"cost_summary"`
	Projects            []string               `json:"projects"`
}

// AnalyticsSummary holds the top-level KPI values.
type AnalyticsSummary struct {
	TotalSessions int `json:"total_sessions"`
	// UniqueProjects counts the projects the *filtered* sessions belong to.
	// AnalyticsReport.Projects is deliberately built before filtering because it
	// populates the project picker, which must keep offering every project; a
	// KPI reading its length reported the whole corpus's project count no matter
	// what the window or the project filter said.
	UniqueProjects           int     `json:"unique_projects"`
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
	Date         string  `json:"date"`
	CacheHitRate float64 `json:"cache_hit_rate"` // 0–100 %
	CachedTokens int     `json:"cached_tokens"`
	// TotalInputTokens is every input-side token in the bucket — fresh input,
	// cache reads and cache writes — which is the denominator CacheHitRate is
	// taken over. It is not the fresh-input count; that is TimeSeriesPoint's
	// InputTokens.
	TotalInputTokens int `json:"total_input_tokens"`
}

// ModelStat describes token distribution across models.
type ModelStat struct {
	Model      string  `json:"model"`
	Tokens     int     `json:"tokens"`
	Percentage float64 `json:"percentage"`
}

// ModelCostStat is one model's share of spend, the answer to the question the
// token breakdown cannot answer. Cost is attributed to the model that spent it,
// including for delegated work — see ClaudeSessionSummary.TotalCostByModel.
type ModelCostStat struct {
	Model string `json:"model"`
	// Provider groups models for display ("Anthropic", "Moonshot"). Derived
	// from the model id, so it needs no catalog lookup and stays correct for a
	// model the catalog has no rate for.
	Provider   string      `json:"provider"`
	Cost       SessionCost `json:"cost"`
	Percentage float64     `json:"percentage"`
	// Sessions is how many sessions this model spent money in — context for a
	// large total, which may be one runaway session or a hundred small ones.
	Sessions int `json:"sessions"`
}

// StackedCostPoint is one time bucket's cost split by model, for the stacked
// cost-over-time chart. The values sum to the same bucket's CostPoint.
type StackedCostPoint struct {
	Date string `json:"date"`
	// CostByModel is keyed by model id. Buckets are independent: a model absent
	// from one bucket spent nothing in it, which the chart renders as zero.
	CostByModel map[string]float64 `json:"cost_by_model"`
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
	filtered := FilterSessions(sessions, p)

	if len(filtered) == 0 {
		return AnalyticsReport{
			TimeSeries:          []TimeSeriesPoint{},
			CacheEfficiency:     []CacheEfficiencyPoint{},
			ModelBreakdown:      []ModelStat{},
			CostByModel:         []ModelCostStat{},
			CostOverTimeByModel: []StackedCostPoint{},
			SessionsPerModel:    []ModelSessionStat{},
			MostActiveDays:      []DayActivity{},
			Heatmap:             []HeatmapCell{},
			HourlyActivity:      buildHourlyActivity(nil, loc),
			CostOverTime:        []CostPoint{},
			Projects:            projects,
		}
	}

	granularity := p.Granularity()
	summary, costSummary := buildSummary(filtered)
	timeSeries := buildTimeSeries(filtered, p.From, p.To, granularity, loc)

	return AnalyticsReport{
		Summary:             summary,
		TimeSeries:          timeSeries,
		CacheEfficiency:     buildCacheEfficiency(timeSeries),
		ModelBreakdown:      buildModelBreakdown(filtered),
		CostByModel:         buildCostByModel(filtered),
		CostOverTimeByModel: buildCostOverTimeByModel(filtered, p.From, p.To, granularity, loc),
		SessionsPerModel:    buildSessionsPerModel(filtered),
		MostActiveDays:      buildMostActiveDays(filtered, loc),
		Heatmap:             buildHeatmap(filtered, loc),
		HourlyActivity:      buildHourlyActivity(filtered, loc),
		CostOverTime:        buildCostOverTime(filtered, p.From, p.To, granularity, loc),
		CostSummary:         costSummary,
		Projects:            projects,
	}
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

// FilterSessions selects the sessions a window covers: last activity within
// [From, To] inclusive, and — when set — matching Project.
//
// It is exported because it is the single definition of "which sessions does
// this window contain", and more than one endpoint needs to agree on that. The
// insights summary used to answer it with its own SQL predicate over a
// different column (start_time), so the same range produced a different session
// count and a different total cost on two dashboards showing the same window.
// Callers that cannot aggregate in memory take the IDs this returns rather than
// re-implementing the predicate.
func FilterSessions(sessions []ClaudeSessionSummary, p AnalyticsParams) []ClaudeSessionSummary {
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

// SessionIDs returns the session IDs of the given summaries, in order.
func SessionIDs(sessions []ClaudeSessionSummary) []string {
	ids := make([]string, 0, len(sessions))
	for _, s := range sessions {
		ids = append(ids, s.SessionID)
	}
	return ids
}

func buildSummary(sessions []ClaudeSessionSummary) (AnalyticsSummary, CostSummary) {
	var totalInput, totalOutput, totalCacheRead, totalCacheWrite int
	modelCount := make(map[string]int)
	projects := make(map[string]struct{})
	var cost CostSummary

	var unknown unknownPricingAccumulator

	for _, s := range sessions {
		projects[s.ProjectPath] = struct{}{}
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
		UniqueProjects:           len(projects),
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

// buildCacheEfficiency derives the per-bucket hit rate from the token series.
//
// The rate comes from CacheHitRate, the single definition this and the insight
// pipeline now share — see that function for why the denominator is every
// input-side token rather than just fresh input.
func buildCacheEfficiency(ts []TimeSeriesPoint) []CacheEfficiencyPoint {
	out := make([]CacheEfficiencyPoint, 0, len(ts))
	for _, p := range ts {
		rate := CacheHitRate(p.InputTokens, p.CacheReadTokens, p.CacheWriteTokens)
		out = append(out, CacheEfficiencyPoint{
			Date:             p.Date,
			CacheHitRate:     math.Round(rate*10000) / 100,
			CachedTokens:     p.CacheReadTokens,
			TotalInputTokens: p.InputTokens + p.CacheReadTokens + p.CacheWriteTokens,
		})
	}
	return out
}

// buildModelBreakdown is the one builder that deliberately does NOT read
// TotalUsage(). Every other aggregate wants "this session's tokens" and does
// not care which model spent them; this one answers "which model did the
// work", so crediting delegated tokens to the delegating parent would make it
// the single chart that cannot answer the question it exists for — whether
// delegation is actually routing work to cheaper models.
//
// So main-thread tokens are attributed to the session's own model and each
// sub-agent's tokens to the model that sub-agent ran. The two together are
// exactly TotalUsage(), so totals and percentages are unchanged; only the
// attribution moves.
func buildModelBreakdown(sessions []ClaudeSessionSummary) []ModelStat {
	tokensByModel := make(map[string]int)
	total := 0
	// add attributes one model's tokens, applying the same synthetic skip and
	// unknown fallback to delegated models as to a session's own.
	//
	// Note the skip is now per model rather than per session. A session whose
	// own model is <synthetic> used to have its delegated tokens dropped along
	// with it; they are real work by a real model and are now counted under
	// that model. No session in the reference corpus is in that position, but
	// the case is reachable, so it is deliberate rather than incidental.
	add := func(model string, u TokenUsage) {
		if model == syntheticModel {
			return // locally generated, never billed — not a real model
		}
		if model == "" {
			model = "unknown"
		}
		t := u.InputTokens + u.OutputTokens
		tokensByModel[model] += t
		total += t
	}
	for _, s := range sessions {
		// Main thread only — the delegated half is attributed per model below,
		// so reading TotalUsage() here would count it twice.
		add(s.Model, s.Usage)

		if len(s.SubagentUsageByModel) > 0 {
			for model, u := range s.SubagentUsageByModel {
				add(model, u)
			}
			continue
		}
		// No per-model breakdown loaded, but the session did delegate: fall
		// back to the parent's model, which is what this builder did before
		// the breakdown existed. Misattributing those tokens is the bug being
		// fixed, but dropping them would be worse — the chart's total would
		// silently stop matching every other total on the dashboard.
		add(s.Model, s.SubagentUsage)
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

// providerPrefixes maps a model-id prefix to the provider that publishes it.
//
// A prefix map rather than a catalog lookup: the provider is a property of the
// identifier, and deriving it here keeps a model with no published rate — the
// case the unpriced bucket exists for — grouped correctly instead of falling
// into "unknown provider" precisely when a reader is trying to find it.
var providerPrefixes = []struct{ prefix, provider string }{
	{"claude-", "Anthropic"},
	{"glm-", "Z.ai"},
	{"qwen", "Alibaba"},
	{"k", "Moonshot"},
}

// providerFor names the provider behind a model id, or "Other" when no prefix
// matches. Matching is longest-prefix-first by declaration order, so the
// single-letter Moonshot prefix is checked last and cannot swallow another
// vendor's id.
func providerFor(model string) string {
	for _, p := range providerPrefixes {
		if strings.HasPrefix(model, p.prefix) {
			return p.provider
		}
	}
	return "Other"
}

// buildCostByModel attributes spend to the model that spent it.
//
// This is the chart the dashboards were missing. The token breakdown beside it
// answers a different question and, on any corpus mixing a caching backend with
// a non-caching one, answers it in a way a reader will misread as spend: cache
// reads and writes are most of the money and none of the tokens that chart
// plots.
//
// Sessions whose rows predate the cost_by_model column contribute nothing here
// until the scanner re-reads them. That is visible as a total below the cost
// summary rather than as a wrong attribution, which is the right way round —
// and it resolves itself on the next scan.
func buildCostByModel(sessions []ClaudeSessionSummary) []ModelCostStat {
	costs := map[string]*SessionCost{}
	sessionCount := map[string]int{}
	total := 0.0

	for _, s := range sessions {
		for model, c := range s.TotalCostByModel() {
			if model == syntheticModel {
				continue // never billed; see buildModelBreakdown
			}
			if costs[model] == nil {
				costs[model] = &SessionCost{}
			}
			costs[model].Add(c)
			sessionCount[model]++
			total += c.TotalUSD
		}
	}

	out := make([]ModelCostStat, 0, len(costs))
	for model, c := range costs {
		pct := 0.0
		if total > 0 {
			pct = math.Round(c.TotalUSD/total*1000) / 10
		}
		out = append(out, ModelCostStat{
			Model:      model,
			Provider:   providerFor(model),
			Cost:       *c,
			Percentage: pct,
			Sessions:   sessionCount[model],
		})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Cost.TotalUSD > out[j].Cost.TotalUSD })
	return out
}

// buildCostOverTimeByModel splits the cost series by model, so "did switching
// models actually change what I spend" is legible over a period rather than
// only in a single-period total.
//
// Buckets follow buildCostOverTime exactly — same key, same walk — so the
// stacked chart and the plain one line up bar for bar.
func buildCostOverTimeByModel(
	sessions []ClaudeSessionSummary, from, to time.Time, granularity string, loc *time.Location,
) []StackedCostPoint {
	buckets := map[string]map[string]float64{}
	for _, s := range sessions {
		key := bucketKey(s.LastActivity, granularity, loc)
		if buckets[key] == nil {
			buckets[key] = map[string]float64{}
		}
		for model, c := range s.TotalCostByModel() {
			if model == syntheticModel {
				continue
			}
			buckets[key][model] += c.TotalUSD
		}
	}

	var result []StackedCostPoint
	walkBuckets(from, to, granularity, loc, func(key string, cur time.Time) {
		costs := buckets[key]
		if costs == nil {
			costs = map[string]float64{}
		}
		result = append(result, StackedCostPoint{Date: bucketLabel(cur, granularity, loc), CostByModel: costs})
	})
	return result
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
