package cmd

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agents-platform-cc-go/internal/config"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/server"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/tools"
)

var serveCmd = &cobra.Command{
	Use:   "serve",
	Short: "Start the HTTP server",
	Long:  "Start the HTTP API server for the agents platform.",
	RunE:  runServe,
}

func init() {
	serveCmd.Flags().Int("port", 3000, "HTTP server port (overrides PORT env var)")
	serveCmd.Flags().String("agents-dir", "", "Directory containing agent YAML files (overrides AGENTS_DIR env var)")
	serveCmd.Flags().String("mcps-file", "", "Path to the MCP registry YAML file (overrides MCPS_FILE env var)")
}

func runServe(cmd *cobra.Command, _ []string) error {
	port := resolvePort(cmd)
	agentsDir := resolveAgentsDir(cmd)
	mcpsFile := resolveMcpsFile(cmd)

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	mcpRegistry, err := config.LoadMCPRegistry(mcpsFile)
	if err != nil {
		return fmt.Errorf("loading MCP registry: %w", err)
	}

	agentRegistry, err := config.LoadAgents(agentsDir, mcpRegistry)
	if err != nil {
		return fmt.Errorf("loading agents: %w", err)
	}

	localToolsMCP, err := tools.StartLocalMCPServer(ctx)
	if err != nil {
		return fmt.Errorf("starting local tools MCP server: %w", err)
	}

	srv := server.New(agentRegistry, mcpRegistry, localToolsMCP, port)

	slugs := make([]string, 0)
	for _, a := range agentRegistry.List() {
		slugs = append(slugs, a.Slug)
	}

	fmt.Fprintf(os.Stderr, "Agents Platform HTTP server running on http://localhost:%d\n", port)
	for _, slug := range slugs {
		fmt.Fprintf(os.Stderr, "  POST /%s/ask         → single JSON answer\n", slug)
		fmt.Fprintf(os.Stderr, "  POST /%s/ask/stream  → SSE streaming\n", slug)
	}
	fmt.Fprintf(os.Stderr, "  GET  /agents              → list agents\n")
	fmt.Fprintf(os.Stderr, "  GET  /health              → health check\n")

	return srv.Run(ctx)
}

func resolvePort(cmd *cobra.Command) int {
	if cmd.Flags().Changed("port") {
		port, _ := cmd.Flags().GetInt("port")
		return port
	}
	if v := os.Getenv("PORT"); v != "" {
		port := 3000
		fmt.Sscanf(v, "%d", &port)
		return port
	}
	return 3000
}

func resolveAgentsDir(cmd *cobra.Command) string {
	if cmd.Flags().Changed("agents-dir") {
		dir, _ := cmd.Flags().GetString("agents-dir")
		return dir
	}
	if v := os.Getenv("AGENTS_DIR"); v != "" {
		return v
	}
	return "./agents"
}

func resolveMcpsFile(cmd *cobra.Command) string {
	if cmd.Flags().Changed("mcps-file") {
		f, _ := cmd.Flags().GetString("mcps-file")
		return f
	}
	if v := os.Getenv("MCPS_FILE"); v != "" {
		return v
	}
	return "./mcps.yaml"
}
