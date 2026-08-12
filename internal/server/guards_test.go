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
		path        string
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
			name:        "multipart is admitted on the upload endpoint",
			method:      http.MethodPost,
			path:        uploadPath,
			contentType: "multipart/form-data; boundary=x",
			body:        body,
			wantStatus:  http.StatusOK,
			wantReached: true,
		},
		{
			// multipart/form-data is itself a CORS-simple type, so admitting it
			// anywhere else would leave every handler reachable cross-origin
			// with nothing but a JSON decode failure standing in the way.
			name:        "multipart is refused everywhere else",
			method:      http.MethodPost,
			contentType: "multipart/form-data; boundary=x",
			body:        body,
			wantStatus:  http.StatusUnsupportedMediaType,
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
			// Body-less is NOT exempt. /chats/{id}/stop,
			// /webhook/regenerate-secret and friends take no body, and a
			// cross-origin POST with neither body nor Content-Type is itself a
			// simple request — exempting it would leave the hole open.
			name:       "a body-less POST is still refused without the header",
			method:     http.MethodPost,
			wantStatus: http.StatusUnsupportedMediaType,
		},
		{
			// The UI sends the header on every request regardless of body, so
			// requiring it always costs nothing.
			name:        "a body-less POST with the header is admitted",
			method:      http.MethodPost,
			contentType: "application/json",
			wantStatus:  http.StatusOK,
			wantReached: true,
		},
		{
			name:       "a body-less DELETE is refused without the header",
			method:     http.MethodDelete,
			wantStatus: http.StatusUnsupportedMediaType,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			var reached bool
			path := tc.path
			if path == "" {
				path = "/api/agents"
			}
			req := httptest.NewRequest(tc.method, path, strings.NewReader(tc.body))
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

// AGENTO_BIND=0.0.0.0 is the documented way to reach Agento from another
// device. Admitting only the literal "0.0.0.0" — which no client ever dials —
// made that setting do nothing: the SPA loaded over the LAN and every /api call
// behind it 403'd, with nothing saying why.
func TestValidateHost_UnspecifiedBindAdmitsIPLiterals(t *testing.T) {
	for _, bind := range []string{"0.0.0.0", "::"} {
		t.Run(bind, func(t *testing.T) {
			srv := &Server{bindAddress: bind}

			for _, host := range []string{"192.168.1.5:8990", "10.0.0.7", "[fd00::1]:8990"} {
				if !srv.hostAllowed(host) {
					t.Errorf("hostAllowed(%q) = false with bind %q, want true — "+
						"the documented escape hatch must work", host, bind)
				}
			}
			// The rebinding property survives: an attacker needs a *name* whose
			// DNS he controls, and a name is never an IP literal.
			if srv.hostAllowed("rebind.evil.example:8990") {
				t.Error("an unspecified bind must not admit arbitrary names")
			}
		})
	}
}

// A specific non-loopback bind admits only that address.
func TestValidateHost_SpecificBindIsNarrow(t *testing.T) {
	srv := &Server{bindAddress: "192.168.1.10"}
	if !srv.hostAllowed("192.168.1.10:8990") {
		t.Error("the bound address must be reachable by that address")
	}
	if srv.hostAllowed("192.168.1.99:8990") {
		t.Error("a specific bind must not admit other addresses")
	}
}

// The Public URL setting is editable at runtime. Reading it once at startup
// meant setting it registered a Telegram webhook under the new host while the
// browser 403'd on every /api call until a restart.
func TestValidateHost_StoredPublicURLIsReadPerRequest(t *testing.T) {
	stored := ""
	srv := &Server{bindAddress: "127.0.0.1", publicURLFunc: func() string { return stored }}

	if srv.hostAllowed("agento.example.com") {
		t.Fatal("nothing stored yet, so the host must be refused")
	}

	stored = "https://agento.example.com"
	if !srv.hostAllowed("agento.example.com") {
		t.Error("a stored Public URL must take effect without a restart")
	}
}

// Neither the Settings field nor SettingsManager validates the value, so a
// scheme-less entry is realistic. url.Parse reports an empty Hostname for it,
// which would leave the user at a 403 wall with a setting that looks right.
func TestHostOf_ToleratesASchemelessValue(t *testing.T) {
	cases := map[string]string{
		"agento.example.com":         "agento.example.com",
		"agento.example.com:8443":    "agento.example.com",
		"https://Agento.Example.com": "agento.example.com",
		"  ":                         "",
	}
	for in, want := range cases {
		if got := hostOf(in); got != want {
			t.Errorf("hostOf(%q) = %q, want %q", in, got, want)
		}
	}
}

// The startup log is the one line users read when the default changes; crying
// wolf on the safest possible value is worse than useless.
func TestIsLoopbackBind_HostnameForms(t *testing.T) {
	cases := map[string]bool{
		"":             true, // default
		"127.0.0.1":    true,
		"::1":          true,
		"localhost":    true,
		"LocalHost":    true,
		"0.0.0.0":      false,
		"192.168.1.10": false,
	}
	for bind, want := range cases {
		if got := (&Server{bindAddress: bind}).IsLoopbackBind(); got != want {
			t.Errorf("IsLoopbackBind(%q) = %v, want %v", bind, got, want)
		}
	}
}
