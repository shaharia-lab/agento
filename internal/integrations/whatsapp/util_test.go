package whatsapp

import (
	"net"
	"strings"
	"testing"
)

func TestValidateMediaURL(t *testing.T) {
	tests := []struct {
		name    string
		url     string
		wantErr bool
		errMsg  string
	}{
		{
			name:    "valid https URL",
			url:     "https://example.com/image.png",
			wantErr: false,
		},
		{
			name:    "valid http URL",
			url:     "http://example.com/image.png",
			wantErr: false,
		},
		{
			name:    "ftp scheme rejected",
			url:     "ftp://example.com/file",
			wantErr: true,
			errMsg:  "unsupported URL scheme",
		},
		{
			name:    "file scheme rejected",
			url:     "file:///etc/passwd",
			wantErr: true,
			errMsg:  "unsupported URL scheme",
		},
		{
			name:    "javascript scheme rejected",
			url:     "javascript:alert(1)",
			wantErr: true,
			errMsg:  "unsupported URL scheme",
		},
		{
			name:    "empty scheme rejected",
			url:     "://example.com/foo",
			wantErr: true,
			errMsg:  "invalid URL",
		},
		{
			name:    "no host rejected",
			url:     "http:///path",
			wantErr: true,
			errMsg:  "URL has no host",
		},
		// Note: private/reserved IP rejection and DNS-based SSRF protection are
		// enforced by safeDialContext at connection time, not here. See
		// TestDownloadMedia for end-to-end SSRF rejection tests.
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := validateMediaURL(tt.url)
			if tt.wantErr {
				if err == nil {
					t.Errorf("validateMediaURL(%q) = nil, want error containing %q", tt.url, tt.errMsg)
					return
				}
				if tt.errMsg != "" {
					if got := err.Error(); !strings.Contains(got, tt.errMsg) {
						t.Errorf("validateMediaURL(%q) error = %q, want it to contain %q", tt.url, got, tt.errMsg)
					}
				}
			} else if err != nil {
				t.Errorf("validateMediaURL(%q) = %v, want nil", tt.url, err)
			}
		})
	}
}

func TestIsPrivateOrReservedIP(t *testing.T) {
	tests := []struct {
		ip   string
		want bool
	}{
		{"127.0.0.1", true},
		{"::1", true},
		{"10.0.0.1", true},
		{"172.16.0.1", true},
		{"192.168.1.1", true},
		{"169.254.1.1", true},
		{"0.0.0.0", true},
		{"8.8.8.8", false},
		{"1.1.1.1", false},
		{"93.184.216.34", false},
	}

	for _, tt := range tests {
		t.Run(tt.ip, func(t *testing.T) {
			ip := net.ParseIP(tt.ip)
			if ip == nil {
				t.Fatalf("invalid test IP: %s", tt.ip)
			}
			got := isPrivateOrReservedIP(ip)
			if got != tt.want {
				t.Errorf("isPrivateOrReservedIP(%s) = %v, want %v", tt.ip, got, tt.want)
			}
		})
	}
}

// TestDownloadMedia verifies end-to-end SSRF rejection via safeDialContext.
// Private/reserved IPs are blocked at dial time (connection time), not at URL
// validation time, which prevents TOCTOU DNS rebinding attacks.
func TestDownloadMedia(t *testing.T) {
	t.Run("rejects private URL", func(t *testing.T) {
		_, err := downloadMedia(t.Context(), "http://127.0.0.1:1234/secret")
		if err == nil {
			t.Fatal("downloadMedia() should reject loopback URL")
		}
		if !strings.Contains(err.Error(), "private/reserved IP") {
			t.Errorf("unexpected error: %v", err)
		}
	})

	t.Run("rejects non-http scheme", func(t *testing.T) {
		_, err := downloadMedia(t.Context(), "ftp://example.com/file")
		if err == nil {
			t.Fatal("downloadMedia() should reject ftp scheme")
		}
		if !strings.Contains(err.Error(), "unsupported URL scheme") {
			t.Errorf("unexpected error: %v", err)
		}
	})

	t.Run("rejects cloud metadata endpoint", func(t *testing.T) {
		_, err := downloadMedia(t.Context(), "http://169.254.169.254/latest/meta-data/")
		if err == nil {
			t.Fatal("downloadMedia() should reject link-local metadata URL")
		}
	})

	t.Run("rejects 10.x private range", func(t *testing.T) {
		_, err := downloadMedia(t.Context(), "http://10.0.0.1/internal")
		if err == nil {
			t.Fatal("downloadMedia() should reject private IP")
		}
	})
}
