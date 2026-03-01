package slack

import (
	"context"
	"fmt"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/config"
)

// Start creates and starts an in-process MCP server for the given Slack integration config.
// Only tools listed in the service's Tools slice are registered. If the Tools slice is empty,
// all tools are registered (backward compatibility).
// The server runs until ctx is canceled.
func Start(ctx context.Context, cfg *config.IntegrationConfig) (claude.McpHTTPServer, error) {
	if !cfg.IsAuthenticated() {
		return claude.McpHTTPServer{}, fmt.Errorf("integration %q has no auth token", cfg.ID)
	}

	token, err := resolveToken(cfg)
	if err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("resolving slack token for %q: %w", cfg.ID, err)
	}

	server := buildMCPServer(cfg, token)

	serverCfg, err := claude.StartInProcessMCPServer(ctx, cfg.ID, server)
	if err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("starting in-process MCP server for %q: %w", cfg.ID, err)
	}

	return serverCfg, nil
}

// resolveToken extracts the bot token from credentials or from an OAuth token stored in Auth.
func resolveToken(cfg *config.IntegrationConfig) (string, error) {
	var creds config.SlackCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return "", fmt.Errorf("parsing slack credentials: %w", err)
	}

	switch creds.AuthMode {
	case "bot_token":
		if creds.BotToken == "" {
			return "", fmt.Errorf("bot_token is empty")
		}
		return creds.BotToken, nil
	case "oauth":
		tok, err := cfg.ParseOAuthToken()
		if err != nil {
			return "", fmt.Errorf("parsing oauth token: %w", err)
		}
		return tok.AccessToken, nil
	default:
		// Fallback: try bot_token from credentials.
		if creds.BotToken != "" {
			return creds.BotToken, nil
		}
		return "", fmt.Errorf("unsupported auth_mode %q and no bot_token available", creds.AuthMode)
	}
}

// buildMCPServer creates the MCP server and registers tools for all enabled services.
func buildMCPServer(cfg *config.IntegrationConfig, token string) *mcp.Server {
	// Build the set of tool names to register from the service configs.
	allowed := make(map[string]bool)
	for _, svc := range cfg.Services {
		if svc.Enabled {
			for _, t := range svc.Tools {
				allowed[t] = true
			}
		}
	}

	serverName := fmt.Sprintf("slack-%s", cfg.ID)
	server := mcp.NewServer(&mcp.Implementation{
		Name:    serverName,
		Version: "1.0.0",
	}, nil)

	// Register tools for the messaging service, filtered by the allowed set.
	if svc, ok := cfg.Services["messaging"]; ok && svc.Enabled {
		registerMessagingTools(server, token, allowed)
	}

	return server
}
