// catalog.json is the embedded seed for the model_pricing table. One entry per
// model family; models whose price never changed carry a single rate with a
// far-past effective_from (a model cannot have been used before it existed, so
// this is exact, not a fudge). Only genuine price changes get additional rows —
// e.g. Sonnet 5's introductory rate reverting on 2026-09-01.
//
// Adding a rate always beats editing one: a new effective_from row leaves
// history priced at the rate that was in force when the tokens were spent.
//
// Rates checked against Anthropic's published pricing on 2026-08-08. Cache
// rates are derived from input at seed time (5m = 1.25×, 1h = 2×, read = 0.1×).
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
}

// builtinPrice carries only input/output; the cache columns are derived.
type builtinPrice struct {
	EffectiveFrom string  `json:"effective_from"`
	InputPerMTok  float64 `json:"input"`
	OutputPerMTok float64 `json:"output"`
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
		if p.InputPerMTok <= 0 || p.OutputPerMTok <= 0 {
			return nil, fmt.Errorf("pricing: catalog entry %q: rates must be positive", e.ModelPattern)
		}
		match := e.MatchType
		if match == "" {
			match = MatchExact
		}
		out = append(out, Rate{
			Provider:            e.Provider,
			ModelPattern:        strings.ToLower(e.ModelPattern),
			MatchType:           match,
			DisplayName:         e.DisplayName,
			InputPerMTok:        p.InputPerMTok,
			OutputPerMTok:       p.OutputPerMTok,
			CacheWrite5mPerMTok: p.InputPerMTok * 1.25,
			CacheWrite1hPerMTok: p.InputPerMTok * 2,
			CacheReadPerMTok:    p.InputPerMTok * 0.1,
			EffectiveFrom:       t,
			Source:              e.Source,
			IsBuiltin:           true,
		})
	}
	return out, nil
}
