// Cross-language vectors for Go's `strconv.Quote`, and for the
// `strconv.IsPrint` table it decides every non-ASCII rune with.
//
// # Why an endpoint needs this at all
//
// The five token validators behind `POST /api/integrations/{id}/auth/validate`
// build the `auth` column with `fmt.Sprintf`, not `json.Marshal`:
//
//	cfg.Auth = json.RawMessage(fmt.Sprintf(`{"validated":true,"team_name":%q}`, teamName))
//
// `%q` on a string is `strconv.Quote`, which is a **Go string literal**, not a
// JSON string — and the two disagree in three ways that all reach this column:
//
//   - `json.Marshal` HTML-escapes `<`, `>` and `&`; `%q` leaves them. A Slack
//     workspace called `A & B` is stored as `"A & B"`, never `"A & B"`.
//   - `%q` escapes a control character as `\x01`, which is **not valid JSON**.
//     Go stores it anyway; a reader that decodes the column then fails. That is
//     Go's behavior and so it is the port's.
//   - the key order is the format string's, not `encoding/json`'s sorted map
//     order — `validated` comes first.
//
// Two of the five payloads interpolate text the port does not control: Jira's
// `displayName` and Slack's `team` are arbitrary Unicode. So the port needs
// `Quote` proper, which means it needs `IsPrint` — and Rust has no equivalent.
// `char::escape_debug`'s notion of printable disagrees with `strconv.IsPrint`
// on **12,589 code points** (it escapes combining marks, which Go prints, and
// prints ~8.5k code points Go escapes), so reusing it would have been wrong
// everywhere a name carried an accent built from a combining mark.
//
// Hence the table travels rather than being transcribed or approximated, the
// same arrangement `migrations_vectors.json` uses: generated from this Go
// toolchain, embedded by `desktop/src-tauri/src/native/goquote.rs` with
// `include_str!`, and asserted by both languages against the same bytes.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestGoQuoteVectors -update-goquote-vectors
//
// A Go release that changed the `IsPrint` table would fail this test rather
// than silently re-pointing the port at a different Unicode version.
package parity

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"strconv"
	"testing"
	"unicode/utf8"
)

const goquoteVectorsFile = "goquote_vectors.json"

var updateGoQuoteVectors = flag.Bool("update-goquote-vectors", false,
	"rewrite goquote_vectors.json from this Go toolchain")

type goquoteVectors struct {
	Comment []string `json:"_comment"`
	// IsPrintRanges is every inclusive [lo, hi] run for which strconv.IsPrint
	// reports true, in ascending order. Ranges rather than code points because
	// the set is ~149k runes and ~711 runs.
	IsPrintRanges [][2]int `json:"is_print_ranges"`
	// Quote pins whole-string results, so the port is checked on the
	// composition of the rules and not only on the table.
	Quote []struct {
		Value string `json:"value"`
		Want  string `json:"want"`
	} `json:"quote"`
	// Auth is the five payloads exactly as the validators spell them, which is
	// the thing this file exists to keep identical.
	Auth []struct {
		Format string `json:"format"`
		Value  string `json:"value"`
		Want   string `json:"want"`
	} `json:"auth"`
}

// quoteInputs cover every branch of appendEscapedRune, in the order that
// function tests them.
var quoteInputs = []string{
	// The ordinary path: printable ASCII, nothing escaped.
	"", "Acme", "acme-corp", "Acme Corp 42", "a_b-c.d",
	// The two runes escaped before printability is even consulted.
	`say "hi"`, `back\slash`, `both "\" here`,
	// Not escaped by %q, unlike json.Marshal — the HTML trio.
	"A & B", "<b>", "a<b>&c",
	// The named escapes, in Go's switch order.
	"\a", "\b", "\f", "\n", "\r", "\t", "\v", "line\nbreak", "tab\there",
	// `\x` — below space, and DEL, which is the one above-space case.
	"\x00", "\x01", "\x1f", "\x7f", "ctrl\x01char",
	// Printable non-ASCII: letters, marks, symbols, CJK, emoji, astral.
	"Ünïcode", "café", "café", "日本語", "Ωμέγα", "emoji🙂", "𝔘𝔫𝔦",
	// Non-printable non-ASCII: `\u` for the BMP, `\U` above it. Spelled as
	// escapes because several are invisible, and one (U+FEFF) is a byte order
	// mark Go's own source parser rejects outright.
	"\u00a0", "\u200b", "\u2028", "\u2029", "\ufeff", "\U000e0001",
	// A lone surrogate cannot exist in a Go string; the replacement char can.
	"\ufffd",
	// Realistic values for the two payloads that interpolate free text.
	"Acme Corp", "Ana María", "Müller & Söhne", "株式会社テスト",
}

// authPayloads are the five `fmt.Sprintf` templates the validators use,
// verbatim from internal/service/integration_service.go.
var authPayloads = []struct {
	format string
	values []string
}{
	{`{"validated":true,"bot_username":%q}`, []string{"my_bot", "Test_Bot"}},
	{`{"validated":true,"display_name":%q}`, []string{"Ana María", "A & B", "ctrl\x01name"}},
	{`{"validated":true,"username":%q}`, []string{"octocat", "some-user"}},
	{`{"validated":true,"team_name":%q}`, []string{"Acme Corp", "Müller & Söhne", "日本語"}},
}

func TestGoQuoteVectors(t *testing.T) {
	want := goquoteVectors{
		Comment: []string{
			"Cross-language parity vectors for Go's strconv.Quote and strconv.IsPrint.",
			"Generated from Go, then frozen. Read by desktop/parity/goquote_parity_test.go",
			"(Go) and by desktop/src-tauri/src/native/goquote.rs (Rust).",
			"is_print_ranges is every inclusive [lo,hi] run where strconv.IsPrint is true.",
			"The port needs it because Rust has no equivalent predicate: char::escape_debug",
			"disagrees with strconv.IsPrint on 12,589 code points.",
			"'want' is exactly what Go produces, so a divergence fails one language against",
			"the other's real output rather than against a belief about it.",
			"Regenerate with: go test ./desktop/parity/ -run TestGoQuoteVectors -update-goquote-vectors",
		},
		IsPrintRanges: isPrintRanges(),
	}

	for _, in := range quoteInputs {
		want.Quote = append(want.Quote, struct {
			Value string `json:"value"`
			Want  string `json:"want"`
		}{in, strconv.Quote(in)})
	}
	for _, payload := range authPayloads {
		for _, v := range payload.values {
			want.Auth = append(want.Auth, struct {
				Format string `json:"format"`
				Value  string `json:"value"`
				Want   string `json:"want"`
			}{payload.format, v, fmt.Sprintf(payload.format, v)})
		}
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateGoQuoteVectors {
		if err := os.WriteFile(goquoteVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", goquoteVectorsFile, err)
		}
		t.Logf("wrote %s (%d IsPrint ranges)", goquoteVectorsFile, len(want.IsPrintRanges))
		return
	}

	frozen, err := os.ReadFile(goquoteVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-goquote-vectors): %v", goquoteVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this Go toolchain produces different results.\n"+
			"Regenerate with -update-goquote-vectors and check what moved — the Rust "+
			"port in native/goquote.rs reads the same file and will fail against it.",
			goquoteVectorsFile)
	}
}

// isPrintRanges walks every valid code point once and collapses the runs.
// Surrogates are skipped: they are not valid runes, cannot appear in a Go
// string, and `IsPrint` reports false for them anyway — including them would
// only add a hole to every range that spans one.
func isPrintRanges() [][2]int {
	var out [][2]int
	lo, prev := -1, -1
	for r := rune(0); r <= utf8.MaxRune; r++ {
		if r >= 0xD800 && r <= 0xDFFF {
			continue
		}
		if !strconv.IsPrint(r) {
			continue
		}
		if lo == -1 {
			lo, prev = int(r), int(r)
			continue
		}
		if int(r) == prev+1 {
			prev = int(r)
			continue
		}
		out = append(out, [2]int{lo, prev})
		lo, prev = int(r), int(r)
	}
	if lo != -1 {
		out = append(out, [2]int{lo, prev})
	}
	return out
}
