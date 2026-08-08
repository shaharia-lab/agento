package pricing

import (
	"strings"
	"time"
)

// Resolver answers (model_id, spent_at) pricing lookups against an in-memory
// snapshot of the catalog. It is read-only after construction — Resolve is
// called once per assistant message across very large transcripts, so it must
// never touch the database. Construct it from Store.Snapshot.
type Resolver struct {
	rates []Rate
}

// NewResolver builds a resolver over rates. The slice is copied so callers
// may reuse theirs.
func NewResolver(rates []Rate) *Resolver {
	cp := make([]Rate, len(rates))
	copy(cp, rates)
	return &Resolver{rates: cp}
}

// Resolved is a rate lookup result: the rate in force, plus whether the answer
// was estimated rather than exact. Estimated is true when the spend predates
// every row for the matched pattern (usage older than the catalog) — the
// earliest rate is the best available answer but was not literally in force.
type Resolved struct {
	Rate      Rate
	Estimated bool
}

// Resolve returns the rate governing modelID at instant at. The boolean is
// false when no pattern matches at all — the caller accounts those tokens in
// the unknown-pricing bucket rather than inventing a cost.
//
// Two-stage selection, both deterministic:
//  1. Pattern specificity — exact beats prefix, ties broken by longer
//     pattern. This is what makes "claude-opus-4-7[1m]" land on the
//     "claude-opus-4-7" prefix rather than some coarser fallback.
//  2. Effective date — among the winning pattern's rows, the newest
//     EffectiveFrom not after at. When every row starts after at (usage
//     predating the catalog), the earliest row is returned with Estimated set,
//     so a seed correction can never silently rewrite history as "exact".
func (r *Resolver) Resolve(modelID string, at time.Time) (Resolved, bool) {
	lower := strings.ToLower(strings.TrimSpace(modelID))
	if lower == "" {
		return Resolved{}, false
	}
	best := r.mostSpecific(lower)
	if best < 0 {
		return Resolved{}, false
	}
	return r.effectiveAt(best, at), true
}

// mostSpecific returns the index of the rate whose pattern matches the
// (already lowercased) model ID most specifically: exact beats prefix, ties
// broken by longer pattern. -1 when nothing matches.
func (r *Resolver) mostSpecific(lower string) int {
	best, bestExact, bestLen := -1, false, -1
	for i := range r.rates {
		if !r.rates[i].matches(lower) {
			continue
		}
		pat := strings.ToLower(r.rates[i].ModelPattern)
		switch {
		case r.rates[i].MatchType == MatchExact && (!bestExact || bestLen < len(pat)):
			best, bestExact, bestLen = i, true, len(pat)
		case r.rates[i].MatchType == MatchPrefix && !bestExact && bestLen < len(pat):
			best, bestLen = i, len(pat)
		}
	}
	return best
}

// matches reports whether the rate's pattern applies to the (already
// lowercased) model ID.
func (r Rate) matches(lower string) bool {
	pat := strings.ToLower(r.ModelPattern)
	if r.MatchType == MatchExact {
		return lower == pat
	}
	return strings.HasPrefix(lower, pat)
}

// effectiveAt picks, among the rows sharing the winning pattern, the newest
// one not after `at` — falling back to the earliest (marked estimated) when
// the spend predates every row.
func (r *Resolver) effectiveAt(best int, at time.Time) Resolved {
	pattern := strings.ToLower(r.rates[best].ModelPattern)
	matchType := r.rates[best].MatchType
	var winner *Rate
	var earliest *Rate
	for i := range r.rates {
		cand := &r.rates[i]
		if strings.ToLower(cand.ModelPattern) != pattern || cand.MatchType != matchType {
			continue
		}
		if earliest == nil || cand.EffectiveFrom.Before(earliest.EffectiveFrom) {
			earliest = cand
		}
		if cand.EffectiveFrom.After(at) {
			continue
		}
		if winner == nil || cand.EffectiveFrom.After(winner.EffectiveFrom) {
			winner = cand
		}
	}
	if winner == nil {
		return Resolved{Rate: *earliest, Estimated: true}
	}
	return Resolved{Rate: *winner}
}
