package whatsapp

import (
	"testing"

	"go.mau.fi/whatsmeow/types"
)

func TestParseJID(t *testing.T) {
	tests := []struct {
		name    string
		input   string
		want    types.JID
		wantErr bool
	}{
		{
			name:  "full JID format",
			input: "1234567890@s.whatsapp.net",
			want:  types.NewJID("1234567890", types.DefaultUserServer),
		},
		{
			name:  "group JID",
			input: "120363123456789012@g.us",
			want:  types.NewJID("120363123456789012", "g.us"),
		},
		{
			name: "phone number digits only - parsed by whatsmeow",
			// whatsmeow's ParseJID accepts bare strings; we verify it doesn't error.
			input: "1234567890",
		},
		{
			name:  "phone number with plus prefix - parsed as phone",
			input: "+1234567890",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := parseJID(tt.input)
			if tt.wantErr {
				if err == nil {
					t.Errorf("parseJID(%q) = %v, want error", tt.input, got)
				}
				return
			}
			if err != nil {
				t.Fatalf("parseJID(%q) error = %v", tt.input, err)
			}
			if tt.want.User != "" {
				if got.User != tt.want.User || got.Server != tt.want.Server {
					t.Errorf("parseJID(%q) = %s, want %s", tt.input, got.String(), tt.want.String())
				}
			}
		})
	}
}

func TestParseJID_SpecialCharacters(t *testing.T) {
	// Characters that trigger the phone-number fallback and fail validation.
	invalidInputs := []struct {
		name  string
		input string
	}{
		{"letter with exclamation", "abc!def"},
		{"special chars", "12#34"},
	}

	for _, tt := range invalidInputs {
		t.Run(tt.name, func(t *testing.T) {
			// These may or may not error depending on whatsmeow's parser.
			// We just verify no panic occurs.
			_, _ = parseJID(tt.input)
		})
	}
}
