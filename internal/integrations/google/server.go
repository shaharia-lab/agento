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
// It registers only the tools that are enabled in cfg.Services.
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

	serverName := fmt.Sprintf("google-%s", cfg.ID)
	server := mcp.NewServer(&mcp.Implementation{
		Name:    serverName,
		Version: "1.0.0",
	}, nil)

	// Register tools for each enabled service.
	if svc, ok := cfg.Services["calendar"]; ok && svc.Enabled {
		registerCalendarTools(server, httpClient)
	}
	if svc, ok := cfg.Services["gmail"]; ok && svc.Enabled {
		registerGmailTools(server, httpClient)
	}
	if svc, ok := cfg.Services["drive"]; ok && svc.Enabled {
		registerDriveTools(server, httpClient)
	}

	serverCfg, err := claude.StartInProcessMCPServer(ctx, cfg.ID, server)
	if err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("starting in-process MCP server for %q: %w", cfg.ID, err)
	}

	return serverCfg, nil
}
