// Package parity holds the cross-language fixtures the desktop app's Rust port
// is checked against.
//
// The desktop app (desktop/, `desktop` branch) is porting this server's
// endpoints to Rust one at a time, and the bar for a ported endpoint is a
// byte-identical JSON response: the frontend is shared, so any field-name,
// escaping or rounding drift is a regression. Go's encoder has three habits
// Rust's does not share by default — `3` for a 3.0 float, HTML escaping of
// `<`, `>` and `&`, and the newline `json.Encoder.Encode` appends — so both
// implementations are pinned to the same vectors.
//
// This half asserts that Go still produces what the frozen file records. The
// other half lives in desktop/src-tauri/src/native/gojson.rs and asserts the
// Rust encoder produces the same text. A divergence in either language fails
// against the other's real output rather than against a belief about it.
package parity

import (
	"bytes"
	"encoding/json"
	"os"
	"strconv"
	"strings"
	"testing"
	"time"
)

const vectorsFile = "gojson_vectors.json"

type gojsonVectors struct {
	Floats []struct {
		Value float64 `json:"value"`
		Want  string  `json:"want"`
	} `json:"floats"`
	Strings []struct {
		Value string `json:"value"`
		Want  string `json:"want"`
	} `json:"strings"`
	CursorFloats []struct {
		Value float64 `json:"value"`
		Want  string  `json:"want"`
	} `json:"cursor_floats"`
	GoTimes []struct {
		Value string `json:"value"`
		Want  string `json:"want"`
	} `json:"go_times"`
}

// encode is exactly what internal/api.Server.writeJSON does, minus the newline
// the vectors file leaves off.
func encode(t *testing.T, v any) string {
	t.Helper()
	var buf bytes.Buffer
	if err := json.NewEncoder(&buf).Encode(v); err != nil {
		t.Fatalf("encoding %v: %v", v, err)
	}
	return strings.TrimSuffix(buf.String(), "\n")
}

func loadVectors(t *testing.T) gojsonVectors {
	t.Helper()
	raw, err := os.ReadFile(vectorsFile)
	if err != nil {
		t.Fatalf("reading %s: %v", vectorsFile, err)
	}
	var v gojsonVectors
	if err := json.Unmarshal(raw, &v); err != nil {
		t.Fatalf("parsing %s: %v", vectorsFile, err)
	}
	if len(v.Floats) == 0 || len(v.Strings) == 0 ||
		len(v.CursorFloats) == 0 || len(v.GoTimes) == 0 {
		t.Fatalf("%s is missing a whole section of vectors", vectorsFile)
	}
	return v
}

func TestGoJSONVectors_Floats(t *testing.T) {
	for _, tc := range loadVectors(t).Floats {
		if got := encode(t, tc.Value); got != tc.Want {
			t.Errorf("encode(%v) = %s, want %s", tc.Value, got, tc.Want)
		}
	}
}

func TestGoJSONVectors_Strings(t *testing.T) {
	for _, tc := range loadVectors(t).Strings {
		if got := encode(t, tc.Value); got != tc.Want {
			t.Errorf("encode(%q) = %s, want %s", tc.Value, got, tc.Want)
		}
	}
}

// TestGoJSONVectors_CursorFloats pins strconv.FormatFloat(_, 'g', -1, 64), the
// spelling the sessions list's keyset cursor uses. It is deliberately not the
// one encoding/json uses — 'g' switches to exponent form at 1e6 rather than
// 1e21 and pads the exponent instead of trimming it — and a cursor minted by
// one implementation is parsed by the other, so the bytes have to agree.
func TestGoJSONVectors_CursorFloats(t *testing.T) {
	for _, tc := range loadVectors(t).CursorFloats {
		if got := strconv.FormatFloat(tc.Value, 'g', -1, 64); got != tc.Want {
			t.Errorf("FormatFloat(%v, 'g') = %s, want %s", tc.Value, got, tc.Want)
		}
	}
}

// TestGoJSONVectors_GoTimes pins the DATETIME round trip. Every timestamp in
// the database is stored as time.Time.String() by the driver, not as RFC 3339,
// so a Rust reader has to parse that layout and render the wire form from it.
func TestGoJSONVectors_GoTimes(t *testing.T) {
	const layout = "2006-01-02 15:04:05.999999999 -0700 MST"
	for _, tc := range loadVectors(t).GoTimes {
		parsed, err := time.Parse(layout, tc.Value)
		if err != nil {
			t.Errorf("parsing %q: %v", tc.Value, err)
			continue
		}
		if got := parsed.UTC().Format(time.RFC3339Nano); got != tc.Want {
			t.Errorf("%q -> %s, want %s", tc.Value, got, tc.Want)
		}
	}
}

// TestGoJSONVectors_CoverTheDivergences fails if someone trims the file down to
// vectors that no longer exercise the three habits the Rust side has to
// reproduce. Without this, the suite could stay green while covering nothing.
func TestGoJSONVectors_CoverTheDivergences(t *testing.T) {
	v := loadVectors(t)

	var sawWholeFloat, sawExponent bool
	for _, tc := range v.Floats {
		if !strings.ContainsAny(tc.Want, ".e") {
			sawWholeFloat = true // a float that Go writes without a fraction
		}
		if strings.Contains(tc.Want, "e") {
			sawExponent = true
		}
	}
	if !sawWholeFloat {
		t.Error("no vector covers a whole-number float (Go writes 3, not 3.0)")
	}
	if !sawExponent {
		t.Error("no vector covers exponent notation")
	}

	var sawHTMLEscape bool
	for _, tc := range v.Strings {
		if strings.Contains(tc.Want, `\u003c`) || strings.Contains(tc.Want, `\u0026`) {
			sawHTMLEscape = true
		}
	}
	if !sawHTMLEscape {
		t.Error("no vector covers HTML escaping of < > &")
	}
}
