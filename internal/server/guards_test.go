package server

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// okHandler records that the request reached the handler behind the guard.
func okHandler(reached *bool) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		*reached = true
		w.WriteHeader(http.StatusOK)
	})
}

// The drive-by chain this issue exists to close: a cross-origin POST carrying
// text/plain is a CORS "simple request", so the browser sends it with no
// preflight and the side effect happens even though the response is unreadable.
// POST /api/agents creates a bypass-permission agent; running it is arbitrary
// Bash. Requiring application/json removes the simple-request status.
func TestRequireJSONContentType(t *testing.T) {
	const body = `{"name":"pwned","slug":"pwned"}`

	tests := []struct {
		name        string
		method      string
		contentType string
		body        string
		wantStatus  int
		wantReached bool
	}{
		{
			name:        "cross-origin shaped POST with text/plain is refused",
			method:      http.MethodPost,
			contentType: "text/plain",
			body:        body,
			wantStatus:  http.StatusUnsupportedMediaType,
		},
		{
			name:        "form-urlencoded is refused",
			method:      http.MethodPost,
			contentType: "application/x-www-form-urlencoded",
			body:        body,
			wantStatus:  http.StatusUnsupportedMediaType,
		},
		{
			name:       "a missing content type on a body is refused",
			method:     http.MethodPost,
			body:       body,
			wantStatus: http.StatusUnsupportedMediaType,
		},
		{
			name:        "application/json is admitted",
			method:      http.MethodPost,
			contentType: "application/json",
			body:        body,
			wantStatus:  http.StatusOK,
			wantReached: true,
		},
		{
			name:        "a charset parameter is tolerated",
			method:      http.MethodPost,
			contentType: "application/json; charset=utf-8",
			body:        body,
			wantStatus:  http.StatusOK,
			wantReached: true,
		},
		{
			// File upload is the one endpoint that legitimately posts
			// something else.
			name:        "multipart upload is admitted",
			method:      http.MethodPost,
			contentType: "multipart/form-data; boundary=x",
			body:        body,
			wantStatus:  http.StatusOK,
			wantReached: true,
		},
		{
			name:        "PUT is guarded too",
			method:      http.MethodPut,
			contentType: "text/plain",
			body:        body,
			wantStatus:  http.StatusUnsupportedMediaType,
		},
		{
			name:        "GET is untouched",
			method:      http.MethodGet,
			wantStatus:  http.StatusOK,
			wantReached: true,
		},
		{
			// A DELETE that sends nothing declares no content type and must
			// still work.
			name:        "a body-less DELETE is untouched",
			method:      http.MethodDelete,
			wantStatus:  http.StatusOK,
			wantReached: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			var reached bool
			req := httptest.NewRequest(tc.method, "/api/agents", strings.NewReader(tc.body))
			if tc.contentType != "" {
				req.Header.Set("Content-Type", tc.contentType)
			}
			rec := httptest.NewRecorder()

			requireJSONContentType(okHandler(&reached)).ServeHTTP(rec, req)

			if rec.Code != tc.wantStatus {
				t.Errorf("status = %d, want %d", rec.Code, tc.wantStatus)
			}
			if reached != tc.wantReached {
				t.Errorf("handler reached = %v, want %v — a refused request must have no side effect",
					reached, tc.wantReached)
			}
		})
	}
}

// DNS rebinding makes an attacker's domain same-origin as far as the browser is
// concerned, so CORS stops applying entirely. A loopback bind does not help.
func TestValidateHost(t *testing.T) {
	srv := &Server{bindAddress: "127.0.0.1", publicHost: "agento.example.com"}

	tests := []struct {
		name  string
		host  string
		allow bool
	}{
		{"localhost with port", "localhost:8990", true},
		{"localhost bare", "localhost", true},
		{"loopback v4", "127.0.0.1:8990", true},
		{"loopback v6", "[::1]:8990", true},
		{"the configured public host", "agento.example.com", true},
		{"the configured public host with a port", "agento.example.com:443", true},
		{"a rebinding attacker's domain", "rebind.evil.example:8990", false},
		{"a lookalike suffix", "notlocalhost", false},
		{"an empty Host", "", false},
		{"a LAN address the server does not bind", "192.168.1.10:8990", false},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := srv.hostAllowed(tc.host); got != tc.allow {
				t.Errorf("hostAllowed(%q) = %v, want %v", tc.host, got, tc.allow)
			}
		})
	}
}

// When the user deliberately binds the LAN, reaching the server by that address
// has to work — otherwise AGENTO_BIND would be a setting that does nothing.
func TestValidateHost_HonorsANonLoopbackBind(t *testing.T) {
	srv := &Server{bindAddress: "192.168.1.10"}
	if !srv.hostAllowed("192.168.1.10:8990") {
		t.Error("a deliberately bound LAN address must be reachable by that address")
	}
	if srv.hostAllowed("rebind.evil.example:8990") {
		t.Error("binding the LAN must not admit arbitrary hosts")
	}
}

func TestListenHostDefaultsToLoopback(t *testing.T) {
	if got := (&Server{}).listenHost(); got != defaultBindAddress {
		t.Errorf("listenHost() = %q, want %q — the default must not be every interface", got, defaultBindAddress)
	}
	if !(&Server{}).IsLoopbackBind() {
		t.Error("the default bind must be loopback")
	}
	if (&Server{bindAddress: "0.0.0.0"}).IsLoopbackBind() {
		t.Error("0.0.0.0 is not loopback")
	}
}

func TestHostOf(t *testing.T) {
	cases := map[string]string{
		"":                             "",
		"https://Agento.Example.com":   "agento.example.com",
		"http://agento.example.com:81": "agento.example.com",
		"://not a url":                 "",
	}
	for in, want := range cases {
		if got := hostOf(in); got != want {
			t.Errorf("hostOf(%q) = %q, want %q", in, got, want)
		}
	}
}
