package server

import (
	"mime"
	"net"
	"net/http"
	"net/url"
	"strings"
)

// Options carries the deployment-shaped settings the router needs.
type Options struct {
	// BindAddress is the interface to listen on. Empty means loopback.
	BindAddress string
	// PublicURL is the externally reachable URL, when the user has configured
	// one. Its host is accepted by validateHost, since a reverse proxy or a
	// Telegram webhook reaches Agento under that name rather than localhost.
	PublicURL string
}

// defaultBindAddress is loopback because Agento ships without authentication
// by design. Listening on every interface put an API that can run arbitrary
// Bash on every network the machine joins.
const defaultBindAddress = "127.0.0.1"

// listenHost resolves the interface to bind.
func (s *Server) listenHost() string {
	if s.bindAddress == "" {
		return defaultBindAddress
	}
	return s.bindAddress
}

// IsLoopbackBind reports whether the server is listening on loopback only.
// Used to phrase the startup log.
func (s *Server) IsLoopbackBind() bool {
	ip := net.ParseIP(s.listenHost())
	return ip != nil && ip.IsLoopback()
}

// hostOf extracts the hostname from a configured URL, or "" when absent or
// unparseable — an unusable value must not widen what validateHost accepts.
func hostOf(rawURL string) string {
	if rawURL == "" {
		return ""
	}
	u, err := url.Parse(rawURL)
	if err != nil {
		return ""
	}
	return strings.ToLower(u.Hostname())
}

// requireJSONContentType rejects a state-changing API request that does not
// declare a JSON body.
//
// This is what closes the drive-by chain, and the mechanism is worth stating
// because it is not obvious: a cross-origin POST carrying Content-Type
// text/plain is a CORS "simple request", so the browser sends it without a
// preflight. The response is unreadable to the attacker, but the side effect
// has already happened — and the handlers decode JSON without checking the
// content type, so the body is parsed regardless. POST /api/agents creates an
// agent with bypass permissions and POST /api/chats/{id}/messages runs it, so
// visiting a page was enough to execute arbitrary Bash on the machine.
//
// Requiring application/json removes the simple-request status: that content
// type forces a preflight, which same-origin CORS then refuses. Binding to
// loopback does not help here, because the browser is already inside.
//
// GET, HEAD and OPTIONS carry no body and are untouched.
//
// A body-less request is deliberately NOT exempt. Several state-changing
// endpoints take no body at all — /chats/{id}/stop, /webhook/regenerate-secret,
// /webhook/register, /duplicate, /claude-sessions/refresh — and a cross-origin
// POST with no body and no Content-Type is itself a simple request, so exempting
// them would leave exactly the hole this middleware exists to close. The UI
// sends the header on every request regardless of body (frontend/src/lib/api.ts),
// so requiring it always costs nothing.
func requireJSONContentType(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !isStateChanging(r.Method) {
			next.ServeHTTP(w, r)
			return
		}

		mediaType, _, err := mime.ParseMediaType(r.Header.Get("Content-Type"))
		if err != nil {
			writeGuardError(w, http.StatusUnsupportedMediaType,
				"a Content-Type of application/json is required")
			return
		}
		if mediaType == "application/json" {
			next.ServeHTTP(w, r)
			return
		}
		// multipart/form-data is itself a CORS-simple type, so it is admitted
		// only for the one endpoint that legitimately posts it. Admitting it
		// everywhere would leave every other handler reachable cross-origin,
		// relying on nothing but a JSON decode failing to prevent the side
		// effect — which is not a security boundary.
		if mediaType == "multipart/form-data" && r.URL.Path == uploadPath {
			next.ServeHTTP(w, r)
			return
		}
		writeGuardError(w, http.StatusUnsupportedMediaType,
			"Content-Type must be application/json")
	})
}

// uploadPath is the one route that legitimately receives multipart bodies.
const uploadPath = "/api/uploads"

// isStateChanging reports whether the method can have side effects.
func isStateChanging(method string) bool {
	switch method {
	case http.MethodPost, http.MethodPut, http.MethodPatch, http.MethodDelete:
		return true
	default:
		return false
	}
}

// validateHost rejects an API request addressed to a name Agento is not served
// under.
//
// Without this, DNS rebinding defeats CORS completely: an attacker's domain
// that resolves to 127.0.0.1 is *same-origin* as far as the browser is
// concerned, so no cross-origin rule applies at all. A loopback bind does not
// help, for the same reason it does not help above.
func (s *Server) validateHost(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !s.hostAllowed(r.Host) {
			writeGuardError(w, http.StatusForbidden,
				"request Host is not one this server is served under")
			return
		}
		next.ServeHTTP(w, r)
	})
}

// hostAllowed reports whether a Host header names this server.
func (s *Server) hostAllowed(rawHost string) bool {
	if rawHost == "" {
		// HTTP/1.0 without a Host. Nothing legitimate reaches the API this way.
		return false
	}
	host := rawHost
	if h, _, err := net.SplitHostPort(rawHost); err == nil {
		host = h
	}
	host = strings.ToLower(strings.Trim(host, "[]"))

	if host == "localhost" {
		return true
	}
	if ip := net.ParseIP(host); ip != nil && ip.IsLoopback() {
		return true
	}
	// The configured public name, for a reverse proxy or a tunnel.
	if s.publicHost != "" && host == s.publicHost {
		return true
	}
	// A deliberately non-loopback bind names the address it listens on, so
	// reaching it over the LAN by IP works when the user asked for that.
	if s.bindAddress != "" && host == strings.ToLower(s.bindAddress) {
		return true
	}
	return false
}

// writeGuardError emits a JSON error in the shape the API already uses.
func writeGuardError(w http.ResponseWriter, status int, msg string) {
	w.Header().Set(headerContentType, contentTypeJSON)
	w.WriteHeader(status)
	// The body is a fixed shape with no user input, so a failed write has
	// nothing to report beyond the status already sent.
	if _, err := w.Write([]byte(`{"error":"` + msg + `"}`)); err != nil {
		return
	}
}
