package whatsapp

import (
	"context"
	"fmt"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/integrations"
)

// NewStarter returns a ServerStarter that captures the dataDir for session storage.
// The whatsmeow session SQLite file is stored at <dataDir>/whatsapp_<id>.db.
func NewStarter(dataDir string) integrations.ServerStarter {
	return func(ctx context.Context, cfg *config.IntegrationConfig) (claude.McpHTTPServer, error) {
		return Start(ctx, cfg, dataDir)
	}
}

// Start creates and starts an in-process MCP server for the given WhatsApp integration config.
// Only tools listed in the service's Tools slice are registered. If the Tools slice is empty,
// all tools are registered (backward compatibility).
// The server runs until ctx is canceled.
func Start(ctx context.Context, cfg *config.IntegrationConfig, dataDir string) (claude.McpHTTPServer, error) {
	if !cfg.IsAuthenticated() {
		return claude.McpHTTPServer{}, fmt.Errorf("integration %q is not paired", cfg.ID)
	}

	client, err := NewClient(ctx, dataDir, cfg.ID, nil)
	if err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("creating whatsapp client for %q: %w", cfg.ID, err)
	}

	if err := client.Connect(); err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("connecting whatsapp client for %q: %w", cfg.ID, err)
	}

	// Disconnect on context cancellation.
	go func() {
		<-ctx.Done()
		client.Disconnect()
	}()

	server := buildMCPServer(cfg, client)

	serverCfg, err := claude.StartInProcessMCPServer(ctx, cfg.ID, server)
	if err != nil {
		client.Disconnect()
		return claude.McpHTTPServer{}, fmt.Errorf("starting in-process MCP server for %q: %w", cfg.ID, err)
	}

	return serverCfg, nil
}

// buildMCPServer creates the MCP server and registers tools for all enabled services.
func buildMCPServer(cfg *config.IntegrationConfig, client *Client) *mcp.Server {
	// Build the set of tool names to register from the service configs.
	allowed := make(map[string]bool)
	for _, svc := range cfg.Services {
		if svc.Enabled {
			for _, t := range svc.Tools {
				allowed[t] = true
			}
		}
	}

	serverName := fmt.Sprintf("whatsapp-%s", cfg.ID)
	server := mcp.NewServer(&mcp.Implementation{
		Name:    serverName,
		Version: "1.0.0",
	}, nil)

	// Register tools for the messaging service, filtered by the allowed set.
	if svc, ok := cfg.Services["messaging"]; ok && svc.Enabled {
		registerMessagingTools(server, client, allowed)
	}

	return server
}
