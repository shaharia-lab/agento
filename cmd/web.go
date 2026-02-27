package cmd

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"os/signal"
	"runtime"
	"syscall"
	"time"

	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/api"
	"github.com/shaharia-lab/agento/internal/build"
	"github.com/shaharia-lab/agento/internal/claudesessions"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/logger"
	"github.com/shaharia-lab/agento/internal/server"
	"github.com/shaharia-lab/agento/internal/service"
	"github.com/shaharia-lab/agento/internal/storage"
	"github.com/shaharia-lab/agento/internal/tools"
)

// NewWebCmd returns the "web" subcommand that starts the HTTP server.
func NewWebCmd(cfg *config.AppConfig) *cobra.Command {
	var port int
	var noBrowser bool

	cmd := &cobra.Command{
		Use:   "web",
		Short: "Start the Agento web UI and API server",
		Long: `Start the Agento HTTP server which serves both the REST API and the
embedded React UI. Open http://localhost:<port> in your browser.`,
		RunE: func(cmd *cobra.Command, _ []string) error {
			// CLI flags override env config.
			if cmd.Flags().Changed("port") {
				cfg.Port = port
			}
			return runWeb(cfg, noBrowser)
		},
	}

	cmd.Flags().IntVar(&port, "port", cfg.Port, "HTTP server port (overrides PORT env var)")
	cmd.Flags().BoolVar(&noBrowser, "no-browser", false, "Do not automatically open the browser on startup")

	return cmd
}

func runWeb(cfg *config.AppConfig, noBrowser bool) error {
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	// Ensure data directories exist.
	for _, dir := range []string{cfg.AgentsDir(), cfg.ChatsDir(), cfg.LogDir()} {
		if err := os.MkdirAll(dir, 0750); err != nil {
			return fmt.Errorf("creating directory %s: %w", dir, err)
		}
	}

	// Create the default working directory if it doesn't exist.
	if err := os.MkdirAll("/tmp/agento/work", 0750); err != nil {
		return fmt.Errorf("creating default working directory: %w", err)
	}

	sysLogger, err := logger.NewSystemLogger(cfg.LogDir(), cfg.SlogLevel())
	if err != nil {
		return fmt.Errorf("initializing logger: %w", err)
	}

	sysLogger.Info("agento starting",
		slog.Int("port", cfg.Port),
		slog.String("data_dir", cfg.DataDir),
		slog.String("version", build.Version),
		slog.String("commit", build.CommitSHA),
		slog.String("build_date", build.BuildDate),
	)

	agentStore := storage.NewFSAgentStore(cfg.AgentsDir())

	// Seed an example agent on first run.
	existing, err := agentStore.List()
	if err != nil {
		return fmt.Errorf("listing agents: %w", err)
	}
	if len(existing) == 0 {
		if err := seedExampleAgent(agentStore); err != nil {
			sysLogger.Warn("could not seed example agent", "error", err)
		}
	}

	mcpRegistry, err := config.LoadMCPRegistry(cfg.MCPsFile())
	if err != nil {
		return fmt.Errorf("loading MCP registry: %w", err)
	}

	localToolsMCP, err := tools.StartLocalMCPServer(ctx)
	if err != nil {
		return fmt.Errorf("starting local tools MCP server: %w", err)
	}

	chatStore := storage.NewFSChatStore(cfg.ChatsDir())

	settingsMgr, err := config.NewSettingsManager(cfg.DataDir, cfg)
	if err != nil {
		return fmt.Errorf("initializing settings: %w", err)
	}

	agentSvc := service.NewAgentService(agentStore, sysLogger)
	chatSvc := service.NewChatService(chatStore, agentStore, mcpRegistry, localToolsMCP, cfg.DefaultModel, sysLogger)

	// Start background scan of ~/.claude/projects so Claude Sessions are available quickly.
	sessionCache := claudesessions.NewCache(sysLogger)
	sessionCache.StartBackgroundScan()

	apiSrv := api.New(agentSvc, chatSvc, settingsMgr, sysLogger, sessionCache)
	srv := server.New(apiSrv, WebFS, cfg.Port, sysLogger)

	url := fmt.Sprintf("http://localhost:%d", cfg.Port)
	sysLogger.Info("server ready", "url", url)

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
	time.Sleep(600 * time.Millisecond)
	ctx := context.Background()
	var c *exec.Cmd
	switch runtime.GOOS {
	case "windows":
		c = exec.CommandContext(ctx, "rundll32", "url.dll,FileProtocolHandler", url)
	case "darwin":
		c = exec.CommandContext(ctx, "open", url)
	default:
		c = exec.CommandContext(ctx, "xdg-open", url)
	}
	_ = c.Start()
}
