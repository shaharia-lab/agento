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
	InsightCards        []InsightCard          `json:"insight_cards"`
	ProjectBreakdown    []ProjectStat          `json:"project_breakdown"`
	ProjectActivity     []ProjectDayActivity   `json:"project_activity"`
	TopSessions         TopSessions            `json:"top_sessions"`
	CostOverTimeByModel []StackedCostPoint     `json:"cost_over_time_by_model"`
	MostActiveDays      []DayActivity          `json:"most_active_days"`
	Heatmap             []HeatmapCell          `json:"heatmap"`
	HourlyActivity      []HourlyActivity       `json:"hourly_activity"`
	CostOverTime        []CostPoint            `json:"cost_over_time"`
	CostSummary         CostSummary            `json:"cost_summary"`
	Projects            []string               `json:"projects"`
	// Granularity is the bucket width every series in this report was built at
	// — "hourly", "daily", "weekly" or "monthly". It travels with the report
	// because a bucket key alone no longer says how wide its bucket is: a
	// weekly and a monthly bucket are both keyed by a YYYY-MM-DD, and a reader
	// deriving a span from the first and last populated key needs to know how
	// far past the last one the data actually reaches.
	Granularity string `json:"granularity"`
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

// ProjectStat aggregates one project's activity over the window.
//
// The project was previously only a filter: nothing anywhere told a user which
// of their projects the money went to, or when they worked on what.
type ProjectStat struct {
	Project  string `json:"project"`
	Sessions int    `json:"sessions"`
	// Tokens is conversation tokens (input+output); TotalTokens includes cache
	// traffic. Both are reported because they answer different questions and
	// conflating them is what made the token headline misleading.
	Tokens       int         `json:"tokens"`
	TotalTokens  int         `json:"total_tokens"`
	Cost         SessionCost `json:"cost"`
	Percentage   float64     `json:"percentage"` // share of the window's cost
	LastActivity time.Time   `json:"last_activity"`
	// FoldedProjects is non-zero only on the "Other projects" row, and says how
	// many projects it stands for. The UI states it rather than presenting a
	// bucket as a project — a chart that showed 20 of 500 bars without saying
	// so would read as the whole picture.
	FoldedProjects int `json:"folded_projects,omitempty"`
}

// ProjectDayActivity is one project's activity in one local time bucket, for
// the "what did I work on when" strip.
//
// Named for the day because that is what it is at every window a reader
// normally looks at; the bucket follows the report's granularity, so a
// multi-year window aggregates by week or month. Following it rather than
// staying daily is what bounds the strip: at eight charted projects a six-year
// daily window would emit ~16,000 cells.
type ProjectDayActivity struct {
	Project  string  `json:"project"`
	Date     string  `json:"date"`
	Sessions int     `json:"sessions"`
	CostUSD  float64 `json:"cost_usd"`
}

// SessionRanking is one row of a leaderboard: enough to recognize the session
// and follow the id to it.
type SessionRanking struct {
	SessionID     string    `json:"session_id"`
	Title         string    `json:"title"`
	Project       string    `json:"project"`
	Model         string    `json:"model"`
	CostUSD       float64   `json:"cost_usd"`
	DurationMs    int64     `json:"duration_ms"`
	Tokens        int       `json:"tokens"`
	SubagentCount int       `json:"subagent_count"`
	LastActivity  time.Time `json:"last_activity"`
}

// TopSessions holds the leaderboards. Each is the same sessions ranked by a
// different measure, because "expensive", "long" and "large" pick out different
// sessions and a user chasing cost wants the first.
type TopSessions struct {
	ByCost     []SessionRanking `json:"by_cost"`
	ByDuration []SessionRanking `json:"by_duration"`
	ByTokens   []SessionRanking `json:"by_tokens"`
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

// The bucket widths a time series can be reported at, coarsest last.
const (
	GranularityHourly  = "hourly"
	GranularityDaily   = "daily"
	GranularityWeekly  = "weekly"
	GranularityMonthly = "monthly"
	GranularityYearly  = "yearly"
)

// maxBuckets is the ceiling every series is designed to stay under, and what
// the granularity thresholds below are chosen to satisfy.
//
// It is not enforced by truncation — a truncated series would be a lie about
// the window — it is a property of the thresholds: 7 days of hours is 169
// buckets, 120 days is 121, 3 years of weeks is 157, 12 years of months is 145,
// and beyond that a year is one bucket. A test walks every one of those bands
// and asserts it, so a threshold edited without the arithmetic fails rather
// than quietly reintroducing a 790 KB payload.
//
// The yearly band exists only because from/to come from a query string: no UI
// offers a twelve-year window, but nothing stops one being typed, and a series
// should degrade in resolution rather than in size.
const maxBuckets = 200

// Granularity picks the bucket width from the window's length.
//
// Before this, every window longer than a week was reported daily: "all time"
// starts in 2020, so an all-time request emitted 2,415 buckets across four
// series — 790 KB of JSON and four Recharts SVGs with thousands of points, on a
// corpus of 798 sessions. That payload grows with the calendar, not with the
// corpus, so it was already the wrong size on the machine it was measured on.
//
// Coarsening rather than truncating: a reader asking for six years wants six
// years, at whatever resolution six years can be read at. Every width still
// produces a YYYY-MM-DD-shaped key (weekly and monthly buckets are keyed by
// their first day), which is the contract analyticsMetrics.ts parses.
func (p AnalyticsParams) Granularity() string {
	span := p.To.Sub(p.From)
	switch {
	case span <= 7*24*time.Hour:
		return GranularityHourly
	case span <= 120*24*time.Hour:
		return GranularityDaily
	case span <= 3*365*24*time.Hour:
		return GranularityWeekly
	case span <= 12*365*24*time.Hour:
		return GranularityMonthly
	default:
		return GranularityYearly
	}
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

	granularity := p.Granularity()
	if len(filtered) == 0 {
		return emptyReport(projects, loc, granularity)
	}

	summary, costSummary := buildSummary(filtered)
	projectBreakdown := buildProjectBreakdown(filtered)
	costByModel := buildCostByModel(filtered)
	timeSeries := buildTimeSeries(filtered, p.From, p.To, granularity, loc)

	return AnalyticsReport{
		Summary:             summary,
		TimeSeries:          timeSeries,
		CacheEfficiency:     buildCacheEfficiency(timeSeries),
		ModelBreakdown:      buildModelBreakdown(filtered),
		CostByModel:         costByModel,
		InsightCards:        buildInsightCards(filtered, costByModel),
		CostOverTimeByModel: buildCostOverTimeByModel(filtered, p.From, p.To, granularity, loc),
		ProjectBreakdown:    projectBreakdown,
		ProjectActivity:     buildProjectActivity(filtered, projectBreakdown, granularity, loc),
		TopSessions:         buildTopSessions(filtered),
		SessionsPerModel:    buildSessionsPerModel(filtered),
		MostActiveDays:      buildMostActiveDays(filtered, loc),
		Heatmap:             buildHeatmap(filtered, loc),
		HourlyActivity:      buildHourlyActivity(filtered, loc),
		CostOverTime:        buildCostOverTime(filtered, p.From, p.To, granularity, loc),
		CostSummary:         costSummary,
		Projects:            projects,
		Granularity:         granularity,
	}
}

// emptyReport is what a window with no sessions returns.
//
// Every slice is empty rather than nil so the JSON carries [] and no consumer
// has to distinguish "no data" from "field missing". Projects is still
// populated: the picker must keep offering every project, or a user who filters
// into an empty window cannot filter back out of it.
func emptyReport(projects []string, loc *time.Location, granularity string) AnalyticsReport {
	return AnalyticsReport{
		TimeSeries:          []TimeSeriesPoint{},
		CacheEfficiency:     []CacheEfficiencyPoint{},
		ModelBreakdown:      []ModelStat{},
		CostByModel:         []ModelCostStat{},
		InsightCards:        []InsightCard{},
		CostOverTimeByModel: []StackedCostPoint{},
		SessionsPerModel:    []ModelSessionStat{},
		MostActiveDays:      []DayActivity{},
		Heatmap:             []HeatmapCell{},
		HourlyActivity:      buildHourlyActivity(nil, loc),
		CostOverTime:        []CostPoint{},
		ProjectBreakdown:    []ProjectStat{},
		ProjectActivity:     []ProjectDayActivity{},
		TopSessions: TopSessions{
			ByCost:     []SessionRanking{},
			ByDuration: []SessionRanking{},
			ByTokens:   []SessionRanking{},
		},
		Projects:    projects,
		Granularity: granularity,
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

// topProjectsCharted bounds the project×day strip. Every project appears in the
// table; the strip shows the busiest, because a 30-row strip is unreadable and
// the tail is visually indistinguishable from empty anyway.
const topProjectsCharted = 8

// topSessionsPerBoard is how many rows each leaderboard carries.
const topSessionsPerBoard = 10

// buildProjectBreakdown aggregates the window by project, ordered by spend.
//
// No schema change was needed: project_path is on every cached row, and cost is
// the stored per-session figure, so this total and the dashboard total are the
// same money by construction.
func buildProjectBreakdown(sessions []ClaudeSessionSummary) []ProjectStat {
	stats := map[string]*ProjectStat{}
	total := 0.0

	for _, s := range sessions {
		p := stats[s.ProjectPath]
		if p == nil {
			p = &ProjectStat{Project: s.ProjectPath}
			stats[s.ProjectPath] = p
		}
		u := s.TotalUsage()
		c := s.TotalCost()
		p.Sessions++
		p.Tokens += u.InputTokens + u.OutputTokens
		p.TotalTokens += u.InputTokens + u.OutputTokens + u.CacheReadTokens + u.CacheCreationTokens
		p.Cost.Add(c)
		if s.LastActivity.After(p.LastActivity) {
			p.LastActivity = s.LastActivity
		}
		total += c.TotalUSD
	}

	out := make([]ProjectStat, 0, len(stats))
	for _, p := range stats {
		if total > 0 {
			p.Percentage = math.Round(p.Cost.TotalUSD/total*1000) / 10
		}
		out = append(out, *p)
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Cost.TotalUSD != out[j].Cost.TotalUSD {
			return out[i].Cost.TotalUSD > out[j].Cost.TotalUSD
		}
		// A tie is common when several projects are entirely unpriced; ordering
		// by name then keeps the response stable across requests.
		return out[i].Project < out[j].Project
	})
	return foldProjectTail(out)
}

// topProjectsListed bounds the project table. Beyond it the tail is folded into
// one row rather than dropped: at 500 projects the table is neither readable
// nor cheap, but a total that quietly excluded 480 of them would be wrong.
const topProjectsListed = 20

// OtherProjectsLabel names the folded tail row. Exported because the UI has to
// recognize it: it is a bucket, not a project, so it must not be clickable as a
// filter and must say how many projects it stands for.
const OtherProjectsLabel = "Other projects"

// foldProjectTail keeps the top projects by spend and sums the rest into one
// row, preserving every figure's total.
//
// Folding rather than truncating, per the no-silent-caps convention: a chart
// that shows 20 of 500 bars without saying so reads as "these are all the
// projects". The row carries the count it stands for so the UI can state it.
func foldProjectTail(ranked []ProjectStat) []ProjectStat {
	if len(ranked) <= topProjectsListed+1 {
		// +1 because folding a single project into "Other (1 project)" is
		// strictly worse than naming it.
		return ranked
	}
	head := ranked[:topProjectsListed]
	tail := ranked[topProjectsListed:]

	other := ProjectStat{Project: OtherProjectsLabel, FoldedProjects: len(tail)}
	for _, p := range tail {
		other.Sessions += p.Sessions
		other.Tokens += p.Tokens
		other.TotalTokens += p.TotalTokens
		other.Cost.Add(p.Cost)
		other.Percentage += p.Percentage
		if p.LastActivity.After(other.LastActivity) {
			other.LastActivity = p.LastActivity
		}
	}
	other.Percentage = math.Round(other.Percentage*10) / 10
	return append(head, other)
}

// buildProjectActivity is the project×bucket strip: which projects were worked
// on when, for the busiest projects in the window. The bucket is the report's
// own granularity, so the strip cannot grow without bound as the window does.
func buildProjectActivity(
	sessions []ClaudeSessionSummary, ranked []ProjectStat, granularity string, loc *time.Location,
) []ProjectDayActivity {
	charted := map[string]struct{}{}
	for i, p := range ranked {
		if i >= topProjectsCharted {
			break
		}
		charted[p.Project] = struct{}{}
	}

	type key struct{ project, date string }
	cells := map[key]*ProjectDayActivity{}
	for _, s := range sessions {
		if _, ok := charted[s.ProjectPath]; !ok {
			continue
		}
		k := key{s.ProjectPath, bucketKey(s.LastActivity, granularity, loc)}
		if cells[k] == nil {
			cells[k] = &ProjectDayActivity{Project: k.project, Date: k.date}
		}
		cells[k].Sessions++
		cells[k].CostUSD += s.TotalCost().TotalUSD
	}

	out := make([]ProjectDayActivity, 0, len(cells))
	for _, c := range cells {
		out = append(out, *c)
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Project != out[j].Project {
			return out[i].Project < out[j].Project
		}
		return out[i].Date < out[j].Date
	})
	return out
}

// buildTopSessions ranks the window's sessions three ways.
//
// The rankings ship with the report rather than being sorted client-side so a
// dashboard is self-contained and the ids can deep-link straight to a session —
// the list page can already filter, but nothing pointed a user at the five
// sessions that cost them the most.
func buildTopSessions(sessions []ClaudeSessionSummary) TopSessions {
	rankings := make([]SessionRanking, 0, len(sessions))
	for _, s := range sessions {
		u := s.TotalUsage()
		// Active time, not the start/last span: ranking by span makes "Longest"
		// a leaderboard of which sessions were resumed after the longest break.
		duration := s.TotalActiveDurationMs()
		rankings = append(rankings, SessionRanking{
			SessionID:     s.SessionID,
			Title:         s.ResolveDisplayTitle(),
			Project:       s.ProjectPath,
			Model:         displayModel(s.Model),
			CostUSD:       s.TotalCost().TotalUSD,
			DurationMs:    duration,
			Tokens:        u.InputTokens + u.OutputTokens + u.CacheReadTokens + u.CacheCreationTokens,
			SubagentCount: s.SubagentCount,
			LastActivity:  s.LastActivity,
		})
	}

	return TopSessions{
		ByCost:     topBy(rankings, func(r SessionRanking) float64 { return r.CostUSD }),
		ByDuration: topBy(rankings, func(r SessionRanking) float64 { return float64(r.DurationMs) }),
		ByTokens:   topBy(rankings, func(r SessionRanking) float64 { return float64(r.Tokens) }),
	}
}

// topBy returns the highest-scoring rankings, dropping zero scores: a
// leaderboard padded with $0.00 rows to reach ten states nothing.
func topBy(rankings []SessionRanking, score func(SessionRanking) float64) []SessionRanking {
	out := make([]SessionRanking, 0, len(rankings))
	for _, r := range rankings {
		if score(r) > 0 {
			out = append(out, r)
		}
	}
	sort.Slice(out, func(i, j int) bool { return score(out[i]) > score(out[j]) })
	if len(out) > topSessionsPerBoard {
		out = out[:topSessionsPerBoard]
	}
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

// maxSessionHourCells bounds how many hour cells one session may occupy.
//
// Two weeks of continuous activity is already far outside anything real; a span
// longer than that is a broken time range, and letting it paint thousands of
// cells would swamp the chart with one bad row. Such a session is attributed to
// the hour it ended, which is what every session used to get.
const maxSessionHourCells = 24 * 14

// walkSessionHours calls fn once for every local hour a session was active,
// with that hour's share of the session's duration.
//
// Bucketing a session at a single instant is what made "Activity by Hour of
// Day" answer a different question than its title: an 8h51m session put all of
// its weight on the hour it *ended*, so the chart showed when work stopped
// rather than when it happened. Spreading it across its span also makes the
// chart agree with its own drill-down, which has always selected sessions whose
// activity window overlaps the clicked hour.
//
// A session with no measurable duration occupies the single hour it happened
// in, with the full weight.
func walkSessionHours(s ClaudeSessionSummary, loc *time.Location, fn func(at time.Time, share float64)) {
	start := s.StartTime.In(loc)
	end := s.LastActivity.In(loc)
	if !end.After(start) {
		fn(start, 1)
		return
	}
	if end.Sub(start) > maxSessionHourCells*time.Hour {
		fn(end, 1)
		return
	}

	total := end.Sub(start)
	for cur := start; cur.Before(end); {
		next := nextLocalHour(cur, loc)
		if next.After(end) {
			next = end
		}
		fn(cur, float64(next.Sub(cur))/float64(total))
		cur = next
	}
}

// nextLocalHour is the start of the hour after t, on t's own wall clock.
//
// Built with time.Date rather than Truncate because Truncate works in UTC: in a
// zone offset by a half or quarter hour (Asia/Kolkata, Asia/Kathmandu) it puts
// the cell boundary at :30 or :45 local, which splits one local hour into two
// cells and counts the session in it twice. time.Date also normalizes a DST
// transition to a real instant, and always advances, so the walk terminates.
func nextLocalHour(t time.Time, loc *time.Location) time.Time {
	local := t.In(loc)
	next := time.Date(local.Year(), local.Month(), local.Day(), local.Hour()+1, 0, 0, 0, loc)
	if !next.After(t) {
		// Only reachable if a zone transition maps the next wall-clock hour to
		// an instant at or before t; stepping an hour keeps the walk moving.
		return t.Add(time.Hour)
	}
	return next
}

func buildHeatmap(sessions []ClaudeSessionSummary, loc *time.Location) []HeatmapCell {
	type cellKey struct{ dow, hour int }
	cells := make(map[cellKey]*HeatmapCell)
	for _, s := range sessions {
		u := s.TotalUsage()
		tokens := u.InputTokens + u.OutputTokens
		walkSessionHours(s, loc, func(at time.Time, share float64) {
			k := cellKey{int(at.Weekday()), at.Hour()}
			if cells[k] == nil {
				cells[k] = &HeatmapCell{DayOfWeek: k.dow, Hour: k.hour}
			}
			// Sessions counts presence — a session active across three hours is
			// one session in each, so the column totals exceed the session count
			// by design. Tokens are shared out, so they still sum to the corpus.
			cells[k].Sessions++
			cells[k].Tokens += int(math.Round(float64(tokens) * share))
		})
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
		u := s.TotalUsage()
		tokens := u.InputTokens + u.OutputTokens
		// Same span-based attribution as the heatmap — see walkSessionHours.
		walkSessionHours(s, loc, func(at time.Time, share float64) {
			hours[at.Hour()].Sessions++
			hours[at.Hour()].Tokens += int(math.Round(float64(tokens) * share))
		})
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

// bucketStart truncates an instant to the start of the bucket that contains it,
// in loc.
//
// One definition serves both bucketKey and walkBuckets, which is what keeps a
// session's bucket and the walked series aligned. When they were separate the
// walk started at the raw window edge, so a weekly or monthly series would emit
// keys no session could ever land in and drop the ones they did.
//
// Weeks start on Monday, matching ISO-8601 and every other weekday-aware figure
// on the dashboard.
func bucketStart(t time.Time, granularity string, loc *time.Location) time.Time {
	t = t.In(loc)
	switch granularity {
	case GranularityHourly:
		return time.Date(t.Year(), t.Month(), t.Day(), t.Hour(), 0, 0, 0, loc)
	case GranularityWeekly:
		// Sunday is 0 in Go's numbering; shift so Monday is the week's first day.
		offset := (int(t.Weekday()) + 6) % 7
		day := time.Date(t.Year(), t.Month(), t.Day(), 0, 0, 0, 0, loc)
		return day.AddDate(0, 0, -offset)
	case GranularityMonthly:
		return time.Date(t.Year(), t.Month(), 1, 0, 0, 0, 0, loc)
	case GranularityYearly:
		return time.Date(t.Year(), 1, 1, 0, 0, 0, 0, loc)
	default:
		return time.Date(t.Year(), t.Month(), t.Day(), 0, 0, 0, 0, loc)
	}
}

// bucketKey derives a session's bucket in loc. Format renders in the time's own
// location, so the conversion has to happen here rather than at the edges — an
// instant is only a day or an hour once you say whose day you mean.
//
// Weekly and monthly buckets are keyed by their first day rather than by a
// "2026-W32" or "2026-08" form, so every key a series can carry still parses as
// the YYYY-MM-DD (optionally T-suffixed with an hour) that analyticsMetrics.ts
// splits and reads.
func bucketKey(t time.Time, granularity string, loc *time.Location) string {
	start := bucketStart(t, granularity, loc)
	if granularity == GranularityHourly {
		return start.Format("2006-01-02T15")
	}
	return start.Format("2006-01-02")
}

func bucketLabel(t time.Time, granularity string, loc *time.Location) string {
	return bucketKey(t, granularity, loc)
}

// walkBuckets calls fn once per bucket from `from` to `to` inclusive, stepping
// in loc.
//
// Steps advance the calendar unit rather than adding a fixed duration: across a
// DST transition a local day is 23 or 25 hours long, and a fixed 24h step drifts
// off the wall clock, duplicating one key and skipping another. The same applies
// to weeks, and months are not a fixed length at all.
func walkBuckets(from, to time.Time, granularity string, loc *time.Location, fn func(string, time.Time)) {
	// Start at the containing bucket, not the raw window edge: a window
	// beginning mid-week must still emit the week its first sessions key into.
	cur := bucketStart(from, granularity, loc)
	end := to.In(loc)
	for !cur.After(end) {
		fn(bucketKey(cur, granularity, loc), cur)
		switch granularity {
		case GranularityHourly:
			cur = cur.Add(time.Hour)
		case GranularityWeekly:
			cur = cur.AddDate(0, 0, 7)
		case GranularityMonthly:
			cur = cur.AddDate(0, 1, 0)
		case GranularityYearly:
			cur = cur.AddDate(1, 0, 0)
		default:
			cur = cur.AddDate(0, 0, 1)
		}
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
