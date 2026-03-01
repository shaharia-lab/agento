package telegram

import (
	"context"
	"fmt"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/config"
)

// Start creates and starts an in-process MCP server for the given Telegram integration config.
// Only tools listed in the service's Tools slice are registered. If the Tools slice is empty,
// all tools are registered (backward compatibility).
// The server runs until ctx is canceled.
func Start(ctx context.Context, cfg *config.IntegrationConfig) (claude.McpHTTPServer, error) {
	if !cfg.IsAuthenticated() {
		return claude.McpHTTPServer{}, fmt.Errorf("integration %q has no auth token", cfg.ID)
	}

	var creds config.TelegramCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("parsing telegram credentials for %q: %w", cfg.ID, err)
	}

	server := buildMCPServer(cfg, creds.BotToken)

	serverCfg, err := claude.StartInProcessMCPServer(ctx, cfg.ID, server)
	if err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("starting in-process MCP server for %q: %w", cfg.ID, err)
	}

	return serverCfg, nil
}

// buildMCPServer creates the MCP server and registers tools for all enabled services.
func buildMCPServer(cfg *config.IntegrationConfig, botToken string) *mcp.Server {
	// Build the set of tool names to register from the service configs.
	allowed := make(map[string]bool)
	for _, svc := range cfg.Services {
		if svc.Enabled {
			for _, t := range svc.Tools {
				allowed[t] = true
			}
		}
	}

	serverName := fmt.Sprintf("telegram-%s", cfg.ID)
	server := mcp.NewServer(&mcp.Implementation{
		Name:    serverName,
		Version: "1.0.0",
	}, nil)

	// Register tools for the messaging service, filtered by the allowed set.
	if svc, ok := cfg.Services["messaging"]; ok && svc.Enabled {
		registerMessagingTools(server, botToken, allowed)
	}

	return server
}
