// Package integrations manages external service integrations (e.g. Google Calendar, Gmail, Drive)
// that run as in-process MCP servers made available to Claude agents.
//
// There are two ways a server here comes to exist, and only one of them is a
// *hosted* server. Start/Reload/Stop keep a long-lived server per integration,
// recorded in the registry; StartFilteredServer builds a throwaway one per
// agent run from a fresh row read, records nothing, and is what every run
// actually uses. AGENTO_INTEGRATIONS switches the first off for this process
// (see hostingDisabled) and deliberately does not touch the second.
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

	// hostingOff reports that this process must not host any integration's
	// long-lived MCP server. Snapshotted at construction from
	// AGENTO_INTEGRATIONS — see hostingDisabled.
	//
	// It gates Start/Reload/Stop and nothing else: StartFilteredServer builds a
	// server per agent run from a fresh row read and never touches the hosted
	// maps, so a run that uses integration tools works exactly as before.
	hostingOff bool
}

// NewRegistry creates a new IntegrationRegistry backed by the given store.
func NewRegistry(store storage.IntegrationStore, logger *slog.Logger) *IntegrationRegistry {
	return &IntegrationRegistry{
		store:      store,
		starters:   make(map[string]ServerStarter),
		servers:    make(map[string]claude.McpHTTPServer),
		cancels:    make(map[string]context.CancelFunc),
		logger:     logger,
		hostingOff: hostingDisabled(),
	}
}

// hostingDisabled reports whether this process has been told not to host
// integration MCP servers.
//
// Read once: it is a deployment fact, not a setting, and re-reading it per call
// would let a server start halfway through a run.
//
// The desktop app sets it, because its Rust shell owns the hosting (#311) and
// two processes hosting one integration is the hazard — an unauthenticated
// loopback listener in the sidecar would keep serving the credential it was
// started with long after the Rust half had been told to reload or stop it.
func hostingDisabled() bool {
	hostingDisabledOnce.Do(func() {
		hostingOff = hostingOffValue(os.Getenv("AGENTO_INTEGRATIONS"))
	})
	return hostingOff
}

// hostingOffValue is the parsing on its own, so it can be tested without the
// sync.Once that makes the real reader deliberately un-resettable.
//
// Unset is **on**: a plain `agento web` must keep hosting its integrations, and
// only a process that has been told otherwise stops. Anything unrecognized is
// also on, for the same reason — a typo in the variable must not silently
// disable every integration.
func hostingOffValue(raw string) bool {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case "off", "0", "false", "disabled":
		return true
	default:
		return false
	}
}

var (
	hostingDisabledOnce sync.Once
	hostingOff          bool
)

// RegisterStarter registers a ServerStarter for a given integration type (e.g. "google").
func (r *IntegrationRegistry) RegisterStarter(integrationType string, starter ServerStarter) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.starters[integrationType] = starter
}

// Start launches in-process MCP servers for all enabled integrations that have a valid auth token.
//
// A no-op when this process has been told not to host them (AGENTO_INTEGRATIONS
// off): the store is not even listed, because there is nothing to do with the
// result.
func (r *IntegrationRegistry) Start(ctx context.Context) error {
	if r.hostingOff {
		return nil
	}

	integrations, err := r.store.List(ctx)
	if err != nil {
		return fmt.Errorf("listing integrations: %w", err)
	}

	for _, cfg := range integrations {
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
// A no-op when hosting is off. This is the single choke point for every caller
// that reacts to a config change — the update handler, the OAuth callback and
// the five token validators all reach a server only through here — so one guard
// covers all of them and no caller has to know.
func (r *IntegrationRegistry) Reload(ctx context.Context, id string) error {
	if r.hostingOff {
		return nil
	}

	r.Stop(id)

	cfg, err := r.store.Get(ctx, id)
	if err != nil {
		return fmt.Errorf("loading integration %q: %w", id, err)
	}
	if cfg == nil {
		return nil // deleted — nothing to start
	}
	if !cfg.Enabled || !cfg.IsAuthenticated() {
		return nil // disabled or not authenticated
	}
	return r.startOne(ctx, cfg)
}

// Stop cancels the running MCP server for the given integration id.
//
// Already a no-op when hosting is off, since nothing was ever recorded — but
// spelled out, so the three lifecycle methods read the same way and a future
// caller of startOne cannot make the maps non-empty behind this one's back.
func (r *IntegrationRegistry) Stop(id string) {
	if r.hostingOff {
		return
	}

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
