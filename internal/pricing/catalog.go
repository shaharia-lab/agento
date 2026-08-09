// catalog.json is the embedded seed for the model_pricing table. One entry per
// model family; models whose price never changed carry a single rate with a
// far-past effective_from (a model cannot have been used before it existed, so
// this is exact, not a fudge). Only genuine price changes get additional rows —
// e.g. Sonnet 5's introductory rate reverting on 2026-09-01.
//
// Adding a rate always beats editing one: a new effective_from row leaves
// history priced at the rate that was in force when the tokens were spent.
//
// Anthropic rates checked against published pricing on 2026-08-08; third-party
// rates on 2026-08-09. Cache rates default to Anthropic's TTL multipliers
// (5m = 1.25×, 1h = 2×, read = 0.1×) and are overridable per rate, because
// other providers publish their own cached-input price and do not split cache
// writes by time-to-live at all.
package pricing

import (
	_ "embed"
	"encoding/json"
	"fmt"
	"strings"
	"time"
)

//go:embed catalog.json
var builtinJSON []byte

// builtinEntry is one model family's seed record in builtin.json.
type builtinEntry struct {
	Provider     string         `json:"provider"`
	ModelPattern string         `json:"model_pattern"`
	MatchType    MatchType      `json:"match_type"`
	DisplayName  string         `json:"display_name"`
	Source       string         `json:"source"`
	Rates        []builtinPrice `json:"rates"`
	// Billable defaults to true when absent. Setting it false marks a model
	// that genuinely costs nothing, which is the only way to seed all-zero
	// rates — otherwise a zero is treated as an unfilled entry and rejected.
	Billable *bool `json:"billable"`
	// Estimated marks an entry whose rates are a best effort rather than the
	// model's published price, e.g. the bare family aliases.
	Estimated bool `json:"estimated"`
}

// billable reports the entry's billable flag, defaulting to true when the JSON
// omits it — every real model is billable, so only the exceptions say so.
func (e builtinEntry) billable() bool {
	return e.Billable == nil || *e.Billable
}

// builtinPrice carries input/output plus optional cache overrides. When a cache
// field is absent the Anthropic TTL multipliers are derived from input; a
// provider that publishes its own cached-input price states it here.
type builtinPrice struct {
	EffectiveFrom string   `json:"effective_from"`
	InputPerMTok  float64  `json:"input"`
	OutputPerMTok float64  `json:"output"`
	CacheWrite5m  *float64 `json:"cache_write_5m,omitempty"`
	CacheWrite1h  *float64 `json:"cache_write_1h,omitempty"`
	CacheRead     *float64 `json:"cache_read,omitempty"`
}

// orDerive returns the explicit override when present, else input × mult.
func orDerive(override *float64, input, mult float64) float64 {
	if override != nil {
		return *override
	}
	return input * mult
}

// farPast stamps models with a single, never-changed rate.
const farPast = "2020-01-01T00:00:00Z"

// BuiltinCatalog parses and normalizes the embedded seed. Cache rates are
// derived from the input rate (1.25× / 2× / 0.1× per #180), and a catalog
// that fails to parse is a build-time authoring error, so this panics.
func BuiltinCatalog() []builtinEntry {
	var entries []builtinEntry
	if err := json.Unmarshal(builtinJSON, &entries); err != nil {
		panic(fmt.Sprintf("pricing: invalid embedded catalog: %v", err))
	}
	return entries
}

// match defaults an unset match type to exact.
func match(mt MatchType) MatchType {
	if mt == "" {
		return MatchExact
	}
	return mt
}

// validateRate enforces the invariant that makes a $0.00 row meaningful: a
// billable model must price every token category above zero, and a
// non-billable one must price them all at exactly zero. Without this a
// half-filled entry — say an output rate someone forgot — would silently
// under-report cost instead of failing the build.
func validateRate(pattern string, r Rate) error {
	cols := []float64{
		r.InputPerMTok, r.OutputPerMTok,
		r.CacheWrite5mPerMTok, r.CacheWrite1hPerMTok, r.CacheReadPerMTok,
	}
	if !r.Billable {
		for _, v := range cols {
			if v != 0 {
				return fmt.Errorf("pricing: rate %q: non-billable rates must all be zero", pattern)
			}
		}
		return nil
	}
	if r.InputPerMTok <= 0 || r.OutputPerMTok <= 0 {
		return fmt.Errorf("pricing: rate %q: rates must be positive", pattern)
	}
	for _, v := range cols {
		if v < 0 {
			return fmt.Errorf("pricing: rate %q: cache rates must not be negative", pattern)
		}
	}
	return nil
}

// rates flattens an entry into seed rows ready for the store.
func (e builtinEntry) rates() ([]Rate, error) {
	out := make([]Rate, 0, len(e.Rates))
	for _, p := range e.Rates {
		from := p.EffectiveFrom
		if from == "" {
			from = farPast
		}
		t, err := time.Parse(time.RFC3339, from)
		if err != nil {
			return nil, fmt.Errorf("pricing: catalog entry %q: bad effective_from %q: %w",
				e.ModelPattern, from, err)
		}
		r := Rate{
			Provider:            e.Provider,
			ModelPattern:        strings.ToLower(e.ModelPattern),
			MatchType:           match(e.MatchType),
			DisplayName:         e.DisplayName,
			InputPerMTok:        p.InputPerMTok,
			OutputPerMTok:       p.OutputPerMTok,
			CacheWrite5mPerMTok: orDerive(p.CacheWrite5m, p.InputPerMTok, 1.25),
			CacheWrite1hPerMTok: orDerive(p.CacheWrite1h, p.InputPerMTok, 2),
			CacheReadPerMTok:    orDerive(p.CacheRead, p.InputPerMTok, 0.1),
			EffectiveFrom:       t,
			Source:              e.Source,
			IsBuiltin:           true,
			Billable:            e.billable(),
			Estimated:           e.Estimated,
		}
		if err := validateRate(e.ModelPattern, r); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, nil
}
