// Cross-language vectors for trigger-rule matching (#319).
//
// `matchRule` decides whether an inbound Telegram message runs an agent, and
// what prompt it runs with. It is a pure function, so it can be pinned exactly —
// and it needs to be, because almost every clause is a place Go and Rust
// disagree by default:
//
//   - **`text[:prefixLen]` is a byte slice.** Go compares bytes and will happily
//     cut a multi-byte character in half; the same index in Rust panics. The
//     length guard is `len(text) < prefixLen` — also bytes — so a prefix longer
//     in bytes than the message is rejected before the slice, but a message that
//     is long enough in bytes while being shorter in characters is not.
//   - **`strings.EqualFold` is Unicode simple folding**, not ASCII
//     case-insensitivity and not lower-casing: sigma folds to final sigma, while
//     U+0130 does *not* fold to ASCII `i` even though it lower-cases to one.
//   - **`strings.TrimSpace` trims `unicode.IsSpace`**, which is not the same set
//     as any ASCII trim.
//   - **An empty prompt after the trim is not a match**, even though the prefix
//     matched — a bare `/ask` with nothing after it runs nothing.
//   - **Keywords are OR, and lower-cased on both sides** with Unicode
//     `ToLower`; chat ids are OR and compared exactly.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestTriggerMatchVectors -update-trigger-match-vectors
package parity

import (
	"encoding/json"
	"flag"
	"os"
	"testing"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/trigger"
)

const triggerMatchVectorsFile = "trigger_match_vectors.json"

var updateTriggerMatchVectors = flag.Bool("update-trigger-match-vectors", false,
	"rewrite "+triggerMatchVectorsFile+" from what Go's matchRule produces")

type triggerMatchCase struct {
	Name string `json:"name"`
	Note string `json:"note"`

	FilterPrefix   string   `json:"filter_prefix"`
	FilterKeywords []string `json:"filter_keywords"`
	FilterChatIDs  []string `json:"filter_chat_ids"`

	Text   string `json:"text"`
	ChatID string `json:"chat_id"`

	Matched bool   `json:"matched"`
	Prompt  string `json:"prompt"`
}

type triggerMatchVectors struct {
	Cases []triggerMatchCase `json:"cases"`
}

func triggerMatchCases() []triggerMatchCase {
	return []triggerMatchCase{
		{
			Name: "no-filters/everything-matches",
			Note: "an unfiltered rule takes the message verbatim as the prompt",
			Text: "hello there", ChatID: "42",
		},
		{
			Name:         "prefix/stripped-and-trimmed",
			Note:         "the prompt is what follows the prefix, space-trimmed",
			FilterPrefix: "/ask", Text: "/ask   what is the time", ChatID: "42",
		},
		{
			Name:         "prefix/case-insensitive",
			Note:         "EqualFold, so the stored prefix and the sent one may differ in case",
			FilterPrefix: "/Ask", Text: "/aSK something", ChatID: "42",
		},
		{
			Name: "prefix/ascii-prefix-slices-into-a-multibyte-first-character",
			Note: "the sharpest case here. The prefix is one ASCII byte and the " +
				"message starts with U+212A (three bytes), so text[:1] is a lone " +
				"continuation byte — invalid UTF-8. Go decodes it as RuneError and " +
				"does not match. A port slicing by *character* would compare U+212A " +
				"against 'K' instead; one slicing by byte in Rust would panic.",
			FilterPrefix: "K", Text: "\u212Aelvin question", ChatID: "42",
		},
		{
			Name: "prefix/multibyte-prefix-against-ascii-text",
			Note: "the mirror of the case above. A three-byte prefix against ASCII " +
				"text takes text[:3] — three whole characters — and EqualFold " +
				"compares three runes against one, so it cannot match however the " +
				"single rune folds. The fold orbit is exercised by the sigma cases " +
				"instead, whose forms are all two bytes.",
			FilterPrefix: "\u212A", Text: "kelvin question", ChatID: "42",
		},
		{
			Name:         "prefix/fold-final-sigma",
			Note:         "EqualFold is simple folding, so final sigma folds to sigma",
			FilterPrefix: "\u03C3", Text: "\u03C2 lower sigma", ChatID: "42",
		},
		{
			Name:         "prefix/fold-capital-sigma",
			Note:         "…and the capital folds to both",
			FilterPrefix: "\u03A3", Text: "\u03C3 sigma", ChatID: "42",
		},
		{
			Name: "prefix/dotted-capital-i-does-not-fold-to-ascii-i",
			Note: "U+0130 has no simple fold to ASCII 'i'. A port lower-casing " +
				"instead of folding would disagree, because ToLower(U+0130) begins " +
				"with one.",
			FilterPrefix: "i", Text: "\u0130stanbul", ChatID: "42",
		},
		{
			Name:         "prefix/no-match",
			Note:         "a message that does not start with the prefix runs nothing",
			FilterPrefix: "/ask", Text: "tell me a joke", ChatID: "42",
		},
		{
			Name: "prefix/empty-remainder-is-not-a-match",
			Note: "a bare prefix with nothing after it is rejected, not run with an " +
				"empty prompt",
			FilterPrefix: "/ask", Text: "/ask", ChatID: "42",
		},
		{
			Name:         "prefix/whitespace-only-remainder-is-not-a-match",
			Note:         "…and neither is one whose remainder trims away",
			FilterPrefix: "/ask", Text: "/ask    ", ChatID: "42",
		},
		{
			Name: "prefix/shorter-than-the-prefix-in-bytes",
			Note: "the length guard is bytes, and it is what stops the slice below " +
				"from being out of range",
			FilterPrefix: "/ask", Text: "/a", ChatID: "42",
		},
		{
			Name: "prefix/multibyte-text-with-ascii-prefix",
			Note: "the byte slice cuts into a multi-byte character: Go compares the " +
				"broken bytes and does not match, where a char-wise port would " +
				"either panic or compare something else",
			FilterPrefix: "ab", Text: "éxyz", ChatID: "42",
		},
		{
			Name:         "prefix/multibyte-prefix-matches-itself",
			Note:         "the same slice is exact when the prefix really is the prefix",
			FilterPrefix: "é", Text: "é accented question", ChatID: "42",
		},
		{
			Name: "prefix/nbsp-remainder-trims-away",
			Note: "unicode.IsSpace includes U+00A0, so a NBSP-only remainder is " +
				"empty and the rule does not fire. Rust's char::is_whitespace agrees " +
				"— both follow White_Space — but that is two standards agreeing, " +
				"which is why it is pinned rather than assumed.",
			FilterPrefix: "/ask", Text: "/ask\u00A0", ChatID: "42",
		},
		{
			Name:         "prefix/nel-remainder-trims-away",
			Note:         "U+0085 is unicode.IsSpace, so this remainder is empty",
			FilterPrefix: "/ask", Text: "/ask\u0085", ChatID: "42",
		},
		{
			Name:           "keywords/or-logic",
			Note:           "any keyword matching is enough",
			FilterKeywords: []string{"deploy", "release"}, Text: "time to release", ChatID: "42",
		},
		{
			Name:           "keywords/case-insensitive-both-sides",
			Note:           "the text and the keyword are both lower-cased",
			FilterKeywords: []string{"DePloY"}, Text: "Please DEPLOY now", ChatID: "42",
		},
		{
			Name:           "keywords/substring-not-word",
			Note:           "Contains, so a keyword inside a longer word matches",
			FilterKeywords: []string{"ploy"}, Text: "redeployment", ChatID: "42",
		},
		{
			Name:           "keywords/none-match",
			FilterKeywords: []string{"deploy"}, Text: "good morning", ChatID: "42",
		},
		{
			Name:           "keywords/empty-list-matches-everything",
			FilterKeywords: []string{}, Text: "anything at all", ChatID: "42",
		},
		{
			Name:          "chat-ids/allowed",
			FilterChatIDs: []string{"42", "77"}, Text: "hi", ChatID: "77",
		},
		{
			Name:          "chat-ids/not-allowed",
			FilterChatIDs: []string{"42"}, Text: "hi", ChatID: "999",
		},
		{
			Name:          "chat-ids/exact-string-compare",
			Note:          "the id is compared as text, so a leading zero is a different chat",
			FilterChatIDs: []string{"042"}, Text: "hi", ChatID: "42",
		},
		{
			Name:          "chat-ids/negative-group-id",
			Note:          "group chats have negative ids and are compared the same way",
			FilterChatIDs: []string{"-1001234567890"}, Text: "hi", ChatID: "-1001234567890",
		},
		{
			Name: "all-three/prefix-strips-before-keywords-are-checked-against-the-FULL-text",
			Note: "the keyword check runs against `text`, not the stripped prompt — " +
				"so a keyword that appears only inside the prefix still matches",
			FilterPrefix: "/deploy", FilterKeywords: []string{"deploy"},
			Text: "/deploy the thing", ChatID: "42",
		},
		{
			Name: "all-three/keyword-only-in-prefix",
			Note: "the sharp edge of the clause above: the prompt has no 'deploy' in " +
				"it at all, and the rule still fires",
			FilterPrefix: "/deploy", FilterKeywords: []string{"deploy"},
			Text: "/deploy now please", ChatID: "42",
		},
		{
			Name:         "all-three/every-filter-satisfied",
			FilterPrefix: "/ask", FilterKeywords: []string{"status"},
			FilterChatIDs: []string{"42"},
			Text:          "/ask what is the status", ChatID: "42",
		},
		{
			Name:         "all-three/chat-id-rejects-an-otherwise-matching-message",
			FilterPrefix: "/ask", FilterKeywords: []string{"status"},
			FilterChatIDs: []string{"42"},
			Text:          "/ask what is the status", ChatID: "43",
		},
	}
}

func TestTriggerMatchVectors(t *testing.T) {
	cases := triggerMatchCases()
	for i := range cases {
		c := &cases[i]
		rule := &config.TriggerRule{
			Enabled:        true,
			FilterPrefix:   c.FilterPrefix,
			FilterKeywords: c.FilterKeywords,
			FilterChatIDs:  c.FilterChatIDs,
		}
		matched, prompt := trigger.MatchRuleForTest(rule, c.Text, c.ChatID)
		c.Matched = matched
		c.Prompt = prompt
	}

	encoded, err := json.MarshalIndent(triggerMatchVectors{Cases: cases}, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateTriggerMatchVectors {
		if writeErr := os.WriteFile(triggerMatchVectorsFile, encoded, 0o600); writeErr != nil {
			t.Fatalf("writing %s: %v", triggerMatchVectorsFile, writeErr)
		}
		return
	}

	stored, err := os.ReadFile(triggerMatchVectorsFile)
	if err != nil {
		t.Fatalf("reading %s: %v (regenerate with -update-trigger-match-vectors)",
			triggerMatchVectorsFile, err)
	}
	if string(stored) != string(encoded) {
		t.Fatalf("%s is stale: Go's matchRule no longer produces it.\n"+
			"Regenerate with: go test ./desktop/parity/ -run TestTriggerMatchVectors "+
			"-update-trigger-match-vectors", triggerMatchVectorsFile)
	}
}
