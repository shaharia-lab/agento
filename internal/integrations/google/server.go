package google

import (
	"context"
	"fmt"
	"net/http"

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
	if !cfg.IsAuthenticated() {
		return claude.McpHTTPServer{}, fmt.Errorf("integration %q has no auth token", cfg.ID)
	}

	httpClient, err := buildHTTPClient(ctx, cfg)
	if err != nil {
		return claude.McpHTTPServer{}, err
	}

	server := buildMCPServer(cfg, httpClient)

	serverCfg, err := claude.StartInProcessMCPServer(ctx, cfg.ID, server)
	if err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("starting in-process MCP server for %q: %w", cfg.ID, err)
	}

	return serverCfg, nil
}

// buildHTTPClient constructs an OAuth2-authenticated HTTP client for the given integration.
func buildHTTPClient(ctx context.Context, cfg *config.IntegrationConfig) (*http.Client, error) {
	var creds config.GoogleCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return nil, fmt.Errorf("parsing google credentials for %q: %w", cfg.ID, err)
	}

	tok, err := cfg.ParseOAuthToken()
	if err != nil {
		return nil, fmt.Errorf("parsing auth token for %q: %w", cfg.ID, err)
	}

	oauthCfg := &oauth2.Config{
		ClientID:     creds.ClientID,
		ClientSecret: creds.ClientSecret,
		Endpoint:     googleoauth.Endpoint,
	}

	// Create a token source that uses the stored token. The oauth2 library handles
	// refresh automatically when the access token expires.
	ts := oauthCfg.TokenSource(ctx, tok)
	return oauth2.NewClient(ctx, ts), nil
}

// buildMCPServer creates the MCP server and registers tools for all enabled services.
func buildMCPServer(cfg *config.IntegrationConfig, httpClient *http.Client) *mcp.Server {
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

	return server
}
