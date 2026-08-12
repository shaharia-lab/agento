package server

import (
	"mime"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
)

// Options carries the deployment-shaped settings the router needs.
type Options struct {
	// BindAddress is the interface to listen on. Empty means loopback.
	BindAddress string
	// PublicURL is the externally reachable URL configured by environment. Its
	// host is accepted by validateHost, since a reverse proxy or a tunnel
	// reaches Agento under that name rather than localhost.
	PublicURL string

	// PublicURLFunc resolves the *stored* public URL on each request. The
	// setting is editable in the UI at runtime, and reading it once at startup
	// meant a user who set it got a Telegram webhook registered under the new
	// host immediately while their browser 403'd on every /api call until the
	// process restarted — with nothing saying why. triggerService.publicURL
	// re-reads it per call for the same reason. Optional; nil is treated as "".
	PublicURLFunc func() string
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
//
// A hostname bind is resolved rather than assumed non-loopback, so
// AGENTO_BIND=localhost does not trigger the "anyone who can reach this
// address" warning — that line is the one users will read when the default
// changes, and crying wolf on the safest possible value is worse than useless.
func (s *Server) IsLoopbackBind() bool {
	host := s.listenHost()
	if ip := net.ParseIP(host); ip != nil {
		return ip.IsLoopback()
	}
	// A hostname bind is not resolved — that would be a DNS lookup on a startup
	// path, for a log line. "localhost" is the only hostname anyone binds to
	// mean loopback, and anything else is treated as non-loopback, which errs
	// toward warning rather than reassuring.
	return strings.EqualFold(host, "localhost")
}

// currentPublicHost returns the public hostname to admit: the environment's,
// else whatever is stored now.
func (s *Server) currentPublicHost() string {
	if s.publicHost != "" {
		return s.publicHost
	}
	if s.publicURLFunc == nil {
		return ""
	}
	return hostOf(s.publicURLFunc())
}

// hostOf extracts the hostname from a configured URL, or "" when absent or
// unparseable — an unusable value must not widen what validateHost accepts.
//
// A scheme-less value is tolerated: neither the Settings field nor
// SettingsManager validates it, so "agento.example.com" is a realistic entry
// and url.Parse reports an empty Hostname for it. Rejecting that silently would
// leave the user staring at a 403 wall with a setting that looks correct.
func hostOf(rawURL string) string {
	rawURL = strings.TrimSpace(rawURL)
	if rawURL == "" {
		return ""
	}
	if !strings.Contains(rawURL, "//") {
		rawURL = "//" + rawURL
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
	// The configured public name, for a reverse proxy or a tunnel. Resolved per
	// request so a value set in the UI takes effect without a restart.
	if ph := s.currentPublicHost(); ph != "" && host == ph {
		return true
	}
	return s.boundAddressAllows(host)
}

// boundAddressAllows reports whether a deliberately non-loopback bind should
// admit this Host.
//
// Without it AGENTO_BIND would be a setting that does nothing: the SPA loads
// over the LAN and every /api call behind it 403s, with nothing saying why.
//
// An unspecified bind (0.0.0.0 or ::) is the documented way to allow other
// devices, and no client ever dials that literal — they dial the machine's LAN
// address, which varies. So any Host that is a bare IP literal is admitted.
// That keeps the rebinding property intact, because rebinding requires a *name*
// whose DNS the attacker controls and a name is never an IP literal.
func (s *Server) boundAddressAllows(host string) bool {
	if s.bindAddress == "" {
		return false
	}
	if ip := net.ParseIP(s.bindAddress); ip != nil && ip.IsUnspecified() {
		return net.ParseIP(host) != nil
	}
	return host == strings.ToLower(s.bindAddress)
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

// listenAddrs returns every address to listen on.
//
// A loopback bind covers **both** loopback families. Binding ":port" used to
// accept IPv4 and IPv6 alike, so narrowing the default to 127.0.0.1 silently
// broke http://[::1]:port — and on a host where "localhost" resolves to ::1
// first, a client that does not fall back gets connection-refused on what looks
// like a working server. That is a second breaking change nobody asked for, so
// loopback means loopback rather than IPv4 loopback.
//
// Any other bind is used verbatim: 0.0.0.0 and :: already cover what the user
// asked for, and a specific address means that address.
func (s *Server) listenAddrs() []string {
	host := s.listenHost()
	port := strconv.Itoa(s.port)

	ip := net.ParseIP(host)
	if strings.EqualFold(host, "localhost") || (ip != nil && ip.IsLoopback()) {
		return []string{
			net.JoinHostPort("127.0.0.1", port),
			net.JoinHostPort("::1", port),
		}
	}
	return []string{net.JoinHostPort(host, port)}
}
