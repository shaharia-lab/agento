// Package integrations manages external service integrations (e.g. Google Calendar, Gmail, Drive)
// that run as in-process MCP servers made available to Claude agents.
//
// There are two ways a server here comes to exist, and only one of them is a
// *hosted* server. Start/Reload/Stop keep a long-lived server per integration,
// recorded in the registry; StartFilteredServer builds a throwaway one per
// agent run from a fresh row read, records nothing, and is what every run
// actually uses. AGENTO_INTEGRATIONS names the integration *types* another
// process hosts and so switches the first off **for those types only** (see
// hostedElsewhereFromEnv); it deliberately does not touch the second.
package integrations

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"strings"
	"sync"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/storage"
)

// ServerStarter is the function signature for starting an integration's MCP server.
// It is injected so that Google-specific code stays in the google sub-package.
type ServerStarter func(ctx context.Context, cfg *config.IntegrationConfig) (claude.McpHTTPServer, error)

// IntegrationRegistry manages running in-process MCP servers for each enabled integration.
type IntegrationRegistry struct {
	mu       sync.RWMutex
	store    storage.IntegrationStore
	starters map[string]ServerStarter // type → starter func
	servers  map[string]claude.McpHTTPServer
	cancels  map[string]context.CancelFunc
	logger   *slog.Logger

	// hostedElsewhere is the set of integration *types* another process hosts,
	// which this one must therefore not. Snapshotted at construction from
	// AGENTO_INTEGRATIONS — see hostedElsewhereFromEnv.
	//
	// It gates Start and Reload and nothing else: StartFilteredServer builds a
	// server per agent run from a fresh row read and never touches the hosted
	// maps, so a run that uses integration tools works exactly as before, and
	// Stop only ever removes a server this process started.
	hostedElsewhere hostedTypes
}

// NewRegistry creates a new IntegrationRegistry backed by the given store.
func NewRegistry(store storage.IntegrationStore, logger *slog.Logger) *IntegrationRegistry {
	return &IntegrationRegistry{
		store:           store,
		starters:        make(map[string]ServerStarter),
		servers:         make(map[string]claude.McpHTTPServer),
		cancels:         make(map[string]context.CancelFunc),
		logger:          logger,
		hostedElsewhere: hostedElsewhereFromEnv(),
	}
}

// hostedTypes is the answer to "does somebody else host this type?".
//
// It is a *set of types* rather than a process-wide bool because a starter is
// not always a pure MCP-server constructor: whatsapp/server.go opens a
// whatsmeow WebSocket, registers the live client in a package global and only
// then returns a server config, so `whatsapp.ConnectionStatus`, the reconnect
// endpoint and QR pairing all read state that exists **only** if this process
// started that integration. Switching hosting off wholesale therefore does not
// cost "a bound port" for WhatsApp; it costs the feature. See #311.
type hostedTypes struct {
	// all is the blunt form: every type is hosted elsewhere. Nothing sets it
	// today — it is what `AGENTO_INTEGRATIONS=off` means, and it only becomes
	// the right thing to send once every type has been ported.
	all bool
	// types is the per-type form, `AGENTO_INTEGRATIONS=off:github,slack`.
	types map[string]bool
}

// has reports whether integrationType is hosted by another process.
func (h hostedTypes) has(integrationType string) bool {
	return h.all || h.types[integrationType]
}

// hostedElsewhereFromEnv reads AGENTO_INTEGRATIONS.
//
// Read once: it is a deployment fact, not a setting, and re-reading it per call
// would let a server start halfway through a run.
//
// The desktop app sets it, because its Rust shell owns the hosting of the types
// it has ported (#311) and two processes hosting one integration is the
// hazard — an unauthenticated loopback listener in the sidecar would keep
// serving the credential it was started with long after the Rust half had been
// told to reload or stop it.
func hostedElsewhereFromEnv() hostedTypes {
	hostedElsewhereOnce.Do(func() {
		hostedElsewhereSnapshot = parseHostedTypes(os.Getenv(envHostingSwitch))
	})
	return hostedElsewhereSnapshot
}

// envHostingSwitch is the variable hostedElsewhereFromEnv reads. Spelled once
// here; the test asserts the literal, so a typo in this constant fails rather
// than silently leaving hosting on everywhere.
const envHostingSwitch = "AGENTO_INTEGRATIONS"

// parseHostedTypes is the parsing on its own, so it can be tested without the
// sync.Once that makes the real reader deliberately un-resettable.
//
// The grammar is one of the four off-words, optionally followed by `:` and a
// comma-separated list of integration types — `off:github` means "github is
// hosted elsewhere, everything else is still mine". A bare off-word means every
// type.
//
// **The list, not a constant shared with the Rust half, is the point.** The
// process that knows which types it hosts is the shell, and it builds this
// value from the very table its own starter dispatch reads
// (`registry::hosting_env_value`), so #313–#317 each add one string in one
// place and the two halves cannot disagree. A hardcoded list here would have to
// be updated in lockstep with a table in another language, and forgetting would
// put two processes on one integration.
//
// Unset is **on**: a plain `agento web` must keep hosting its integrations, and
// only a process that has been told otherwise stops. Anything unrecognized is
// also on, for the same reason — a typo in the variable must not silently
// disable every integration. So is an empty list (`off:`), which is what a
// shell that hosts nothing yet would send.
func parseHostedTypes(raw string) hostedTypes {
	word, list, hasList := strings.Cut(strings.ToLower(strings.TrimSpace(raw)), ":")
	switch strings.TrimSpace(word) {
	case "off", "0", "false", "disabled":
	default:
		return hostedTypes{}
	}
	if !hasList {
		return hostedTypes{all: true}
	}

	types := make(map[string]bool)
	for _, name := range strings.Split(list, ",") {
		if name = strings.TrimSpace(name); name != "" {
			types[name] = true
		}
	}
	if len(types) == 0 {
		return hostedTypes{}
	}
	return hostedTypes{types: types}
}

var (
	hostedElsewhereOnce     sync.Once
	hostedElsewhereSnapshot hostedTypes
)

// RegisterStarter registers a ServerStarter for a given integration type (e.g. "google").
func (r *IntegrationRegistry) RegisterStarter(integrationType string, starter ServerStarter) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.starters[integrationType] = starter
}

// Start launches in-process MCP servers for all enabled integrations that have a valid auth token.
//
// Integrations whose type another process hosts are skipped. When *every* type
// is (AGENTO_INTEGRATIONS=off, with no list) the store is not even listed,
// because there is nothing to do with the result.
func (r *IntegrationRegistry) Start(ctx context.Context) error {
	if r.hostedElsewhere.all {
		return nil
	}

	integrations, err := r.store.List(ctx)
	if err != nil {
		return fmt.Errorf("listing integrations: %w", err)
	}

	for _, cfg := range integrations {
		if r.hostedElsewhere.has(cfg.Type) {
			continue
		}
		if !cfg.Enabled || !cfg.IsAuthenticated() {
			continue
		}
		if err := r.startOne(ctx, cfg); err != nil {
			r.logger.Warn("failed to start integration server",
				"id", cfg.ID,
				"type", cfg.Type,
				"error", err,
			)
			// Continue with other integrations rather than failing all.
		}
	}
	return nil
}

// Reload stops and restarts the MCP server for the integration with the given id.
//
// A no-op for a type another process hosts. This is the single choke point for
// every caller that reacts to a config change — the update handler, the OAuth
// callback and the five token validators all reach a server only through here —
// so one guard covers all of them and no caller has to know.
//
// The gate is applied to the *stored row's* type, after the Stop, which is what
// makes a `PUT` that changes an integration's type behave: the server this
// process is holding goes either way, and only the start is conditional.
func (r *IntegrationRegistry) Reload(ctx context.Context, id string) error {
	r.Stop(id)

	if r.hostedElsewhere.all {
		return nil // nothing to start, and nothing to read to decide that
	}

	cfg, err := r.store.Get(ctx, id)
	if err != nil {
		return fmt.Errorf("loading integration %q: %w", id, err)
	}
	if cfg == nil {
		return nil // deleted — nothing to start
	}
	if r.hostedElsewhere.has(cfg.Type) {
		return nil // somebody else's to reload
	}
	if !cfg.Enabled || !cfg.IsAuthenticated() {
		return nil // disabled or not authenticated
	}
	return r.startOne(ctx, cfg)
}

// Stop cancels the running MCP server for the given integration id.
//
// **Deliberately ungated**, unlike Start and Reload. Stopping is only ever
// removing a server *this* process started, so for a type hosted elsewhere the
// maps are empty and this is already a no-op — while a guard here would be the
// one thing able to make a server unstoppable, if the type of a row ever
// changed under a server this process was holding. Nor could the guard be
// applied accurately: Stop takes no context and so cannot look the row's type
// up.
func (r *IntegrationRegistry) Stop(id string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if cancel, ok := r.cancels[id]; ok {
		cancel()
		delete(r.cancels, id)
	}
	delete(r.servers, id)
}

// GetServerConfig returns the McpHTTPServer config for the given integration id.
func (r *IntegrationRegistry) GetServerConfig(id string) (claude.McpHTTPServer, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	cfg, ok := r.servers[id]
	return cfg, ok
}

// AllServerConfigs returns a snapshot of all running server configs keyed by integration id.
func (r *IntegrationRegistry) AllServerConfigs() map[string]claude.McpHTTPServer {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make(map[string]claude.McpHTTPServer, len(r.servers))
	for id, cfg := range r.servers {
		out[id] = cfg
	}
	return out
}

// StartFilteredServer starts a new MCP server for the given integration with only the
// specified tools registered. The server runs until ctx is canceled, so callers should
// pass a session-scoped context for automatic cleanup.
// This is used by agents that only need a subset of an integration's tools.
func (r *IntegrationRegistry) StartFilteredServer(
	ctx context.Context, id string, tools []string,
) (claude.McpHTTPServer, error) {
	cfg, err := r.store.Get(ctx, id)
	if err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("loading integration %q: %w", id, err)
	}
	if cfg == nil {
		return claude.McpHTTPServer{}, fmt.Errorf("integration %q not found", id)
	}
	if !cfg.Enabled || !cfg.IsAuthenticated() {
		return claude.McpHTTPServer{}, fmt.Errorf("integration %q is not enabled or not authenticated", id)
	}

	r.mu.RLock()
	starter, ok := r.starters[cfg.Type]
	r.mu.RUnlock()
	if !ok {
		return claude.McpHTTPServer{}, fmt.Errorf("no starter registered for integration type %q", cfg.Type)
	}

	// Build a filtered copy of the config with only the requested tools.
	filtered := filterConfigTools(cfg, tools)
	return starter(ctx, filtered)
}

// filterConfigTools returns a shallow copy of cfg whose Services only contain
// the tools present in the requested list.
func filterConfigTools(cfg *config.IntegrationConfig, tools []string) *config.IntegrationConfig {
	if len(tools) == 0 {
		return cfg
	}

	want := make(map[string]bool, len(tools))
	for _, t := range tools {
		want[t] = true
	}

	out := *cfg
	out.Services = make(map[string]config.ServiceConfig, len(cfg.Services))
	for svcName, svc := range cfg.Services {
		if !svc.Enabled {
			continue
		}
		var kept []string
		for _, t := range svc.Tools {
			if want[t] {
				kept = append(kept, t)
			}
		}
		if len(kept) > 0 {
			out.Services[svcName] = config.ServiceConfig{
				Enabled: true,
				Tools:   kept,
			}
		}
	}
	return &out
}

// AllowedToolNames returns fully qualified tool names ("mcp__<id>__<tool>") for the given
// integration id and bare tool names.
func AllowedToolNames(id string, tools []string) []string {
	result := make([]string, 0, len(tools))
	for _, t := range tools {
		result = append(result, fmt.Sprintf("mcp__%s__%s", id, t))
	}
	return result
}

// startOne starts the MCP server for a single integration config.
// Caller must NOT hold the mutex.
func (r *IntegrationRegistry) startOne(parentCtx context.Context, cfg *config.IntegrationConfig) error {
	starter, ok := r.starters[cfg.Type]
	if !ok {
		return fmt.Errorf("no starter registered for integration type %q", cfg.Type)
	}

	serverCtx, cancel := context.WithCancel(parentCtx)
	serverCfg, err := starter(serverCtx, cfg)
	if err != nil {
		cancel()
		return fmt.Errorf("starting %q server: %w", cfg.Type, err)
	}

	r.mu.Lock()
	r.servers[cfg.ID] = serverCfg
	r.cancels[cfg.ID] = cancel
	r.mu.Unlock()

	r.logger.Info("integration MCP server started", "id", cfg.ID, "type", cfg.Type, "url", serverCfg.URL)
	return nil
}
