package server

import (
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/api"
)

func discardLogger() *slog.Logger { return slog.New(slog.NewTextHandler(io.Discard, nil)) }

// stubWebhooks stands in for the Telegram webhook handler, which is mounted at
// the root rather than under /api.
type stubWebhooks struct{ reached *bool }

func (s stubWebhooks) Mount(r chi.Router) {
	r.Post("/webhooks/telegram/{id}", func(w http.ResponseWriter, _ *http.Request) {
		*s.reached = true
		w.WriteHeader(http.StatusOK)
	})
}

// newTestRouter builds the real route tree. The api.Server is zero-valued,
// which is enough: every request here is either refused by a guard before it
// reaches a handler, or is routed to the stub webhook.
func newTestRouter(t *testing.T, opts Options, hookReached *bool) http.Handler {
	t.Helper()
	srv := New(&api.Server{}, nil, 8990, discardLogger(), nil,
		stubWebhooks{reached: hookReached}, opts)
	return srv.httpServer.Handler
}

// The guards are only useful if they are actually mounted on /api. Testing the
// middlewares in isolation cannot show that — removing them from the route
// leaves such tests passing.
func TestRouter_GuardsAreMountedOnAPI(t *testing.T) {
	var hookReached bool
	h := newTestRouter(t, Options{}, &hookReached)

	// The drive-by shape: a simple-request content type carrying JSON.
	req := httptest.NewRequest(http.MethodPost, "/api/agents", strings.NewReader(`{"name":"x"}`))
	req.Header.Set("Content-Type", "text/plain")
	req.Host = "localhost:8990"
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusUnsupportedMediaType {
		t.Errorf("POST /api/agents with text/plain = %d, want 415 — the guard is not mounted", rec.Code)
	}

	// The rebinding shape: a foreign Host on a well-formed JSON request.
	req = httptest.NewRequest(http.MethodPost, "/api/agents", strings.NewReader(`{"name":"x"}`))
	req.Header.Set("Content-Type", "application/json")
	req.Host = "rebind.evil.example"
	rec = httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusForbidden {
		t.Errorf("POST /api/agents with a foreign Host = %d, want 403", rec.Code)
	}
}

// The webhook arrives from Telegram's servers under the public hostname and
// with whatever content type Telegram sends. Guarding it would break inbound
// triggers in production — it is not a hole, it authenticates with its own
// secret token.
func TestRouter_WebhookIsNotGuarded(t *testing.T) {
	var hookReached bool
	h := newTestRouter(t, Options{}, &hookReached)

	req := httptest.NewRequest(http.MethodPost, "/webhooks/telegram/abc",
		strings.NewReader(`{"update_id":1}`))
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Host = "tunnel.example.com"
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK || !hookReached {
		t.Errorf("webhook: status %d reached=%v — the guards must not apply at the root",
			rec.Code, hookReached)
	}
}

// /health is what a process supervisor polls; it must not need a Host it does
// not know to send.
func TestRouter_HealthIsNotGuarded(t *testing.T) {
	var hookReached bool
	h := newTestRouter(t, Options{}, &hookReached)

	req := httptest.NewRequest(http.MethodGet, "/health", nil)
	req.Host = "whatever.example"
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Errorf("GET /health = %d, want 200", rec.Code)
	}
}

// A configured PublicURL is how Agento is reached behind a proxy or a tunnel.
func TestRouter_PublicURLHostIsAdmitted(t *testing.T) {
	var hookReached bool
	h := newTestRouter(t, Options{PublicURL: "https://agento.example.com"}, &hookReached)

	req := httptest.NewRequest(http.MethodGet, "/api/agents", nil)
	req.Host = "agento.example.com"
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)

	if rec.Code == http.StatusForbidden {
		t.Error("a configured PublicURL host must be admitted")
	}
}
