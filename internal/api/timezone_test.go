package api

import (
	"testing"
	"time"
)

// TestParseTimezone covers the fallback contract: analytics is a read-only
// dashboard, so an unrecognized or absent zone renders in UTC rather than
// failing the request — which is exactly what every caller got before the
// parameter existed.
func TestParseTimezone(t *testing.T) {
	tests := []struct {
		name string
		in   string
		want string
	}{
		{"empty falls back to UTC", "", "UTC"},
		{"garbage falls back to UTC", "Not/AZone", "UTC"},
		{"nonsense falls back to UTC", "🙂", "UTC"},
		{"a real zone resolves", "Europe/Berlin", "Europe/Berlin"},
		{"UTC resolves to itself", "UTC", "UTC"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := parseTimezone(tt.in).String(); got != tt.want {
				t.Errorf("parseTimezone(%q) = %q, want %q", tt.in, got, tt.want)
			}
		})
	}
}

// TestParseTimezone_EmbeddedTzdata is the acceptance criterion for shipping the
// zone database in the binary: these must resolve with no system zoneinfo, as
// in a distroless or scratch container.
func TestParseTimezone_EmbeddedTzdata(t *testing.T) {
	for _, name := range []string{
		"Europe/Berlin", "America/Los_Angeles", "Asia/Tokyo", "Australia/Sydney",
	} {
		if got := parseTimezone(name).String(); got != name {
			t.Errorf("parseTimezone(%q) = %q — the tzdata import is missing", name, got)
		}
	}
}

// TestParseAnalyticsDate covers the two shapes the endpoint accepts. A bare
// date names a day and must be read in the requesting zone; an RFC3339 value
// states its own offset and is taken at its word.
func TestParseAnalyticsDate(t *testing.T) {
	berlin, err := time.LoadLocation("Europe/Berlin")
	if err != nil {
		t.Fatalf("LoadLocation: %v", err)
	}

	got, err := parseAnalyticsDate("2026-08-08", berlin)
	if err != nil {
		t.Fatalf("parse bare date: %v", err)
	}
	want := time.Date(2026, 8, 8, 0, 0, 0, 0, berlin)
	if !got.Equal(want) {
		t.Errorf("bare date = %s, want local midnight %s", got, want)
	}
	// Berlin is UTC+2 in August, so local midnight is 22:00Z the day before.
	if got.UTC().Day() != 7 {
		t.Errorf("bare date in UTC = %s, want the 7th — local midnight precedes UTC midnight", got.UTC())
	}

	got, err = parseAnalyticsDate("2026-08-08T12:00:00Z", berlin)
	if err != nil {
		t.Fatalf("parse RFC3339: %v", err)
	}
	if !got.Equal(time.Date(2026, 8, 8, 12, 0, 0, 0, time.UTC)) {
		t.Errorf("RFC3339 = %s, want its own stated offset honored", got)
	}

	if _, err := parseAnalyticsDate("the eighth", berlin); err == nil {
		t.Error("an unparseable date must return an error so the caller keeps its default")
	}
}
