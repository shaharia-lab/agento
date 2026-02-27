package google

import (
	"context"
	"fmt"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"
	"golang.org/x/oauth2"
	googleoauth "golang.org/x/oauth2/google"

	"github.com/shaharia-lab/agento/internal/config"
)

// Start creates and starts an in-process MCP server for the given Google integration config.
// Only tools listed in each service's Tools slice are registered. If a service has an empty
// Tools slice, all tools for that service are registered (backward compatibility).
// The server runs until ctx is canceled.
func Start(ctx context.Context, cfg *config.IntegrationConfig) (claude.McpHTTPServer, error) {
	if cfg.Auth == nil {
		return claude.McpHTTPServer{}, fmt.Errorf("integration %q has no auth token", cfg.ID)
	}

	oauthCfg := &oauth2.Config{
		ClientID:     cfg.Credentials.ClientID,
		ClientSecret: cfg.Credentials.ClientSecret,
		Endpoint:     googleoauth.Endpoint,
	}

	// Create a token source that uses the stored token. The oauth2 library handles
	// refresh automatically when the access token expires.
	ts := oauthCfg.TokenSource(ctx, cfg.Auth)
	httpClient := oauth2.NewClient(ctx, ts)

	// Build the set of tool names to register from the service configs.
	allowed := make(map[string]bool)
	for _, svc := range cfg.Services {
		if svc.Enabled {
			for _, t := range svc.Tools {
				allowed[t] = true
			}
		}
	}

	serverName := fmt.Sprintf("google-%s", cfg.ID)
	server := mcp.NewServer(&mcp.Implementation{
		Name:    serverName,
		Version: "1.0.0",
	}, nil)

	// Register tools for each enabled service, filtered by the allowed set.
	if svc, ok := cfg.Services["calendar"]; ok && svc.Enabled {
		registerCalendarTools(server, httpClient, allowed)
	}
	if svc, ok := cfg.Services["gmail"]; ok && svc.Enabled {
		registerGmailTools(server, httpClient, allowed)
	}
	if svc, ok := cfg.Services["drive"]; ok && svc.Enabled {
		registerDriveTools(server, httpClient, allowed)
	}

	serverCfg, err := claude.StartInProcessMCPServer(ctx, cfg.ID, server)
	if err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("starting in-process MCP server for %q: %w", cfg.ID, err)
	}

	return serverCfg, nil
}
