// Package integrations manages external service integrations (e.g. Google Calendar, Gmail, Drive)
// that run as in-process MCP servers made available to Claude agents.
package integrations

import (
	"context"
	"fmt"
	"log/slog"
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
}

// NewRegistry creates a new IntegrationRegistry backed by the given store.
func NewRegistry(store storage.IntegrationStore, logger *slog.Logger) *IntegrationRegistry {
	return &IntegrationRegistry{
		store:    store,
		starters: make(map[string]ServerStarter),
		servers:  make(map[string]claude.McpHTTPServer),
		cancels:  make(map[string]context.CancelFunc),
		logger:   logger,
	}
}

// RegisterStarter registers a ServerStarter for a given integration type (e.g. "google").
func (r *IntegrationRegistry) RegisterStarter(integrationType string, starter ServerStarter) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.starters[integrationType] = starter
}

// Start launches in-process MCP servers for all enabled integrations that have a valid auth token.
func (r *IntegrationRegistry) Start(ctx context.Context) error {
	integrations, err := r.store.List()
	if err != nil {
		return fmt.Errorf("listing integrations: %w", err)
	}

	for _, cfg := range integrations {
		if !cfg.Enabled || cfg.Auth == nil {
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
func (r *IntegrationRegistry) Reload(ctx context.Context, id string) error {
	r.Stop(id)

	cfg, err := r.store.Get(id)
	if err != nil {
		return fmt.Errorf("loading integration %q: %w", id, err)
	}
	if cfg == nil {
		return nil // deleted — nothing to start
	}
	if !cfg.Enabled || cfg.Auth == nil {
		return nil // disabled or not authenticated
	}
	return r.startOne(ctx, cfg)
}

// Stop cancels the running MCP server for the given integration id.
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
