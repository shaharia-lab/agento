package github

import (
	"context"
	"fmt"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/config"
)

// Start creates and starts an in-process MCP server for the given GitHub
// integration config. Only tools listed in each service's Tools slice are
// registered. The server runs until ctx is canceled.
func Start(
	ctx context.Context, cfg *config.IntegrationConfig,
) (claude.McpHTTPServer, error) {
	if !cfg.IsAuthenticated() {
		return claude.McpHTTPServer{}, fmt.Errorf(
			"integration %q has no auth token", cfg.ID)
	}

	var creds config.GitHubCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf(
			"parsing github credentials for %q: %w", cfg.ID, err)
	}

	server := buildMCPServer(cfg, creds.PersonalAccessToken)

	serverCfg, err := claude.StartInProcessMCPServer(ctx, cfg.ID, server)
	if err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf(
			"starting in-process MCP server for %q: %w", cfg.ID, err)
	}

	return serverCfg, nil
}

// buildAllowedSet collects the tool names that should be registered from
// all enabled service configs.
func buildAllowedSet(cfg *config.IntegrationConfig) map[string]bool {
	allowed := make(map[string]bool)
	for _, svc := range cfg.Services {
		if svc.Enabled {
			for _, t := range svc.Tools {
				allowed[t] = true
			}
		}
	}
	return allowed
}

// serviceEnabled returns true if the named service is present and enabled.
func serviceEnabled(cfg *config.IntegrationConfig, name string) bool {
	svc, ok := cfg.Services[name]
	return ok && svc.Enabled
}

// buildMCPServer creates the MCP server and registers tools for all
// enabled services.
func buildMCPServer(cfg *config.IntegrationConfig, token string) *mcp.Server {
	allowed := buildAllowedSet(cfg)

	server := mcp.NewServer(&mcp.Implementation{
		Name:    fmt.Sprintf("github-%s", cfg.ID),
		Version: "1.0.0",
	}, nil)

	if serviceEnabled(cfg, "repos") {
		registerReposTools(server, token, allowed)
	}
	if serviceEnabled(cfg, "issues") {
		registerIssuesTools(server, token, allowed)
	}
	if serviceEnabled(cfg, "pull_requests") {
		registerPullsTools(server, token, allowed)
	}
	if serviceEnabled(cfg, "actions") {
		registerActionsTools(server, token, allowed)
	}
	if serviceEnabled(cfg, "releases") {
		registerReleasesTools(server, token, allowed)
	}

	return server
}
