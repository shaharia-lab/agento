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
	"strings"
	"testing"
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
	if len(v.Floats) == 0 || len(v.Strings) == 0 {
		t.Fatalf("%s has no vectors to check", vectorsFile)
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
