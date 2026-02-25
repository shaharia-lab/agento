package tools

import (
	"context"
	"fmt"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"
)

// LocalMCPServerName is the MCP server name used for all in-process local tools.
// MCP tool names will be mcp__<LocalMCPServerName>__<tool-name>.
const LocalMCPServerName = "local-tools"

// LocalMCPConfig holds the running in-process MCP server configuration.
type LocalMCPConfig struct {
	// ServerCfg is the claude SDK config to pass to WithMcpServers.
	ServerCfg claude.McpHTTPServer
	// ToolNames are the names of all tools registered in the server.
	ToolNames []string
}

// AllowedToolName returns the fully qualified tool name for use with WithAllowedTools.
func (c *LocalMCPConfig) AllowedToolName(toolName string) string {
	return fmt.Sprintf("mcp__%s__%s", LocalMCPServerName, toolName)
}

// AllowedToolNames returns the fully qualified names for a subset of local tools.
// If names is empty, returns qualified names for all tools.
func (c *LocalMCPConfig) AllowedToolNames(names []string) []string {
	result := make([]string, 0, len(names))
	for _, name := range names {
		result = append(result, c.AllowedToolName(name))
	}
	return result
}

// StartLocalMCPServer creates and starts the in-process MCP server with all local tools.
// The server is bound to a random local port and runs until ctx is cancelled.
func StartLocalMCPServer(ctx context.Context) (*LocalMCPConfig, error) {
	server := mcp.NewServer(&mcp.Implementation{
		Name:    LocalMCPServerName,
		Version: "1.0.0",
	}, nil)

	// Register all local tools here.
	mcp.AddTool(server, &mcp.Tool{
		Name:        "current_time",
		Description: "Returns the current date and time for a given IANA timezone (e.g. UTC, America/New_York, Asia/Tokyo). Defaults to UTC.",
	}, getCurrentTime)

	cfg, err := claude.StartInProcessMCPServer(ctx, LocalMCPServerName, server)
	if err != nil {
		return nil, fmt.Errorf("starting local MCP server: %w", err)
	}

	return &LocalMCPConfig{
		ServerCfg: cfg,
		ToolNames: []string{"current_time"},
	}, nil
}
