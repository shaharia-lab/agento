package trigger

import "github.com/shaharia-lab/agento/internal/config"

// MatchRuleForTest exposes `matchRule` for the cross-language vectors in
// `desktop/parity`, which are generated from this implementation and asserted
// against the Rust port.
//
// Not in an `export_test.go`: that file would only be compiled for this
// package's own tests, and the vector generator lives in another package. The
// matcher is a pure function over a rule and a message, so exposing it costs
// nothing — it reads no state and touches no store.
func MatchRuleForTest(rule *config.TriggerRule, text, chatID string) (bool, string) {
	return matchRule(rule, text, chatID)
}
