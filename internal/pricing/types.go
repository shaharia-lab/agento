// Package pricing maintains the model pricing catalog: a persisted set of
// effective-dated rates, an embedded built-in seed, and the resolver that maps
// (model_id, spent_at) to the rate in force at that moment.
//
// The catalog exists so that historical cost figures do not move when a rate
// is corrected: adding a new rate row never rewrites an old one, and lookups
// are answered by the newest row whose effective_from predates the spend.
// Rates are data (SQLite, seeded from catalog.json), not compiled-in code, so
// maintaining them no longer requires a release.
package pricing

import "time"

// MatchType controls how a Rate's ModelPattern is compared to a model ID.
// Exact and prefix matching are deliberately anchored — a rate must never
// attach to a model that merely contains its pattern as a substring.
type MatchType string

const (
	// MatchExact requires the model ID to equal the pattern (case-insensitive).
	MatchExact MatchType = "exact"
	// MatchPrefix requires the model ID to begin with the pattern, so dated
	// snapshots ("claude-haiku-4-5-20251001") and context variants
	// ("claude-opus-4-7[1m]") resolve to their family's rate.
	MatchPrefix MatchType = "prefix"
)

// Rate is one effective-dated price row for a model. All prices are USD per
// million tokens. Prompt-cache writes are billed by time-to-live (1.25× input
// for the 5-minute tier, 2× for the 1-hour tier); the built-in catalog derives
// the cache columns from the input rate unless the provider publishes its own,
// which non-Anthropic providers generally do.
type Rate struct {
	ID                  int64     `json:"id"`
	Provider            string    `json:"provider"`
	ModelPattern        string    `json:"model_pattern"`
	MatchType           MatchType `json:"match_type"`
	DisplayName         string    `json:"display_name"`
	InputPerMTok        float64   `json:"input_per_mtok"`
	OutputPerMTok       float64   `json:"output_per_mtok"`
	CacheWrite5mPerMTok float64   `json:"cache_write_5m_per_mtok"`
	CacheWrite1hPerMTok float64   `json:"cache_write_1h_per_mtok"`
	CacheReadPerMTok    float64   `json:"cache_read_per_mtok"`
	EffectiveFrom       time.Time `json:"effective_from"`
	// Source records where the rate came from (provider pricing page and the
	// date it was checked) so a later maintainer can verify or correct it.
	Source    string `json:"source"`
	IsBuiltin bool   `json:"is_builtin"`
	// UserModified marks a row the user has edited; startup re-seeding never
	// overwrites it.
	UserModified bool `json:"user_modified"`
	// Billable distinguishes a model that deliberately costs nothing — Claude
	// Code's <synthetic> placeholder, embedding models — from one whose rates
	// simply have not been filled in. Both price at $0.00, but only the latter
	// is a gap; a non-billable model is resolved, so it never reaches the
	// unknown-pricing bucket.
	Billable bool `json:"billable"`
	// Estimated marks a rate that is not the model's published price: the bare
	// family aliases ("opus", "sonnet") name no concrete model, so they are
	// priced at the current flagship of that tier as a best effort. Resolved
	// reports this alongside the predates-the-catalog case.
	Estimated bool `json:"estimated"`
}

// Usage is the token consumption of one assistant message, with cache
// creation already split by TTL (the tiers bill differently).
type Usage struct {
	InputTokens           int
	OutputTokens          int
	CacheCreation5mTokens int
	CacheCreation1hTokens int
	CacheReadTokens       int
}

// Cost is the USD cost of some usage, broken down by token category.
type Cost struct {
	InputCostUSD      float64 `json:"input_cost_usd"`
	OutputCostUSD     float64 `json:"output_cost_usd"`
	CacheReadCostUSD  float64 `json:"cache_read_cost_usd"`
	CacheWriteCostUSD float64 `json:"cache_write_cost_usd"`
	TotalCostUSD      float64 `json:"total_cost_usd"`
}

// Price computes the cost of u under r. Costs are independent of the time
// dimension — the caller resolves the rate first.
func (r Rate) Price(u Usage) Cost {
	c := Cost{
		InputCostUSD:     float64(u.InputTokens) / 1_000_000 * r.InputPerMTok,
		OutputCostUSD:    float64(u.OutputTokens) / 1_000_000 * r.OutputPerMTok,
		CacheReadCostUSD: float64(u.CacheReadTokens) / 1_000_000 * r.CacheReadPerMTok,
		CacheWriteCostUSD: float64(u.CacheCreation5mTokens)/1_000_000*r.CacheWrite5mPerMTok +
			float64(u.CacheCreation1hTokens)/1_000_000*r.CacheWrite1hPerMTok,
	}
	c.TotalCostUSD = c.InputCostUSD + c.OutputCostUSD + c.CacheReadCostUSD + c.CacheWriteCostUSD
	return c
}

// Add accumulates another cost into c.
func (c *Cost) Add(o Cost) {
	c.InputCostUSD += o.InputCostUSD
	c.OutputCostUSD += o.OutputCostUSD
	c.CacheReadCostUSD += o.CacheReadCostUSD
	c.CacheWriteCostUSD += o.CacheWriteCostUSD
	c.TotalCostUSD += o.TotalCostUSD
}
