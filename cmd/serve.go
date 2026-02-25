package cmd

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"time"

	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agents-platform-cc-go/internal/api"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/config"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/server"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/storage"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/tools"
)

var serveCmd = &cobra.Command{
	Use:   "serve",
	Short: "Start the Agento web UI and API server",
	Long: `Start the Agento HTTP server which serves both the REST API and the
embedded React UI. Open http://localhost:<port> in your browser.`,
	RunE: runServe,
}

func init() {
	serveCmd.Flags().Int("port", 8080, "HTTP server port (overrides PORT env var)")
	serveCmd.Flags().String("data-dir", "", "Data directory for agents and chats (default: ~/.agento)")
	serveCmd.Flags().Bool("no-browser", false, "Do not automatically open the browser on startup")
}

func runServe(cmd *cobra.Command, _ []string) error {
	port := resolvePort(cmd)
	dataDir := resolveDataDir(cmd)
	noBrowser, _ := cmd.Flags().GetBool("no-browser")

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	// Ensure data directories exist
	agentsDir := filepath.Join(dataDir, "agents")
	chatsDir := filepath.Join(dataDir, "chats")
	for _, dir := range []string{agentsDir, chatsDir} {
		if err := os.MkdirAll(dir, 0755); err != nil {
			return fmt.Errorf("creating directory %s: %w", dir, err)
		}
	}

	agentStore := storage.NewFSAgentStore(agentsDir)

	// Seed an example agent on first run
	existing, err := agentStore.List()
	if err != nil {
		return fmt.Errorf("listing agents: %w", err)
	}
	if len(existing) == 0 {
		if err := seedExampleAgent(agentStore); err != nil {
			fmt.Fprintf(os.Stderr, "warning: could not seed example agent: %v\n", err)
		}
	}

	// Load optional MCP registry
	mcpsFile := filepath.Join(dataDir, "mcps.yaml")
	mcpRegistry, err := config.LoadMCPRegistry(mcpsFile)
	if err != nil {
		return fmt.Errorf("loading MCP registry: %w", err)
	}

	localToolsMCP, err := tools.StartLocalMCPServer(ctx)
	if err != nil {
		return fmt.Errorf("starting local tools MCP server: %w", err)
	}

	chatStore := storage.NewFSChatStore(chatsDir)
	apiSrv := api.New(agentStore, chatStore, mcpRegistry, localToolsMCP)
	srv := server.New(apiSrv, WebFS, port)

	url := fmt.Sprintf("http://localhost:%d", port)
	fmt.Fprintf(os.Stderr, "Agento is running at %s\n", url)
	fmt.Fprintf(os.Stderr, "Data directory: %s\n", dataDir)

	if !noBrowser {
		go openBrowser(url)
	}

	return srv.Run(ctx)
}

func seedExampleAgent(store storage.AgentStore) error {
	agent := &config.AgentConfig{
		Name:        "Hello World",
		Slug:        "hello-world",
		Description: "A friendly assistant to help you get started with Agento.",
		Model:       "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
		Thinking:    "adaptive",
		SystemPrompt: "You are a friendly and helpful assistant. " +
			"You help users understand and use the Agento AI agents platform. " +
			"Today is {{current_date}}.",
		Capabilities: config.AgentCapabilities{
			BuiltIn: []string{"current_time"},
		},
	}
	return store.Save(agent)
}

func openBrowser(url string) {
	time.Sleep(600 * time.Millisecond) // brief delay so server is ready
	var c *exec.Cmd
	switch runtime.GOOS {
	case "windows":
		c = exec.Command("rundll32", "url.dll,FileProtocolHandler", url)
	case "darwin":
		c = exec.Command("open", url)
	default:
		c = exec.Command("xdg-open", url)
	}
	_ = c.Start()
}

func resolvePort(cmd *cobra.Command) int {
	if cmd.Flags().Changed("port") {
		port, _ := cmd.Flags().GetInt("port")
		return port
	}
	if v := os.Getenv("PORT"); v != "" {
		var port int
		if _, err := fmt.Sscanf(v, "%d", &port); err == nil {
			return port
		}
	}
	return 8080
}

func resolveDataDir(cmd *cobra.Command) string {
	if cmd.Flags().Changed("data-dir") {
		dir, _ := cmd.Flags().GetString("data-dir")
		return expandHome(dir)
	}
	if v := os.Getenv("AGENTO_DATA_DIR"); v != "" {
		return expandHome(v)
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return ".agento"
	}
	return filepath.Join(home, ".agento")
}

// resolveAgentsDir is kept for the `ask` command's backward-compatible --agents-dir flag.
func resolveAgentsDir(cmd *cobra.Command) string {
	if cmd.Flags().Changed("agents-dir") {
		dir, _ := cmd.Flags().GetString("agents-dir")
		return expandHome(dir)
	}
	if v := os.Getenv("AGENTS_DIR"); v != "" {
		return expandHome(v)
	}
	// Default: same location as serve uses
	home, err := os.UserHomeDir()
	if err != nil {
		return "./agents"
	}
	return filepath.Join(home, ".agento", "agents")
}

// resolveMcpsFile is kept for the `ask` command's backward-compatible --mcps-file flag.
func resolveMcpsFile(cmd *cobra.Command) string {
	if cmd.Flags().Changed("mcps-file") {
		f, _ := cmd.Flags().GetString("mcps-file")
		return expandHome(f)
	}
	if v := os.Getenv("MCPS_FILE"); v != "" {
		return expandHome(v)
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "./mcps.yaml"
	}
	return filepath.Join(home, ".agento", "mcps.yaml")
}

func expandHome(path string) string {
	if strings.HasPrefix(path, "~/") {
		home, err := os.UserHomeDir()
		if err == nil {
			return filepath.Join(home, path[2:])
		}
	}
	return path
}
