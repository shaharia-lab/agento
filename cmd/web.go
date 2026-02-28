package cmd

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"log"
	"log/slog"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime"
	"syscall"
	"time"

	"github.com/charmbracelet/lipgloss"
	"github.com/muesli/termenv"
	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/api"
	"github.com/shaharia-lab/agento/internal/build"
	"github.com/shaharia-lab/agento/internal/claudesessions"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/eventbus"
	"github.com/shaharia-lab/agento/internal/integrations"
	googleintegration "github.com/shaharia-lab/agento/internal/integrations/google"
	"github.com/shaharia-lab/agento/internal/logger"
	"github.com/shaharia-lab/agento/internal/notification"
	"github.com/shaharia-lab/agento/internal/scheduler"
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

			serverURL := fmt.Sprintf("http://localhost:%d", cfg.Port)
			logFile := filepath.Join(cfg.LogDir(), "system.log")
			printBanner(build.Version, serverURL, logFile)

			if runWeb(cfg, noBrowser) != nil {
				fmt.Fprintf(os.Stderr, "An error occurred. Please check the logs at: %s\n", logFile)
				os.Exit(1)
			}
			return nil
		},
	}

	cmd.Flags().IntVar(&port, "port", cfg.Port, "HTTP server port (overrides PORT env var)")
	cmd.Flags().BoolVar(&noBrowser, "no-browser", false, "Do not automatically open the browser on startup")

	return cmd
}

func runWeb(cfg *config.AppConfig, noBrowser bool) error {
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	if err := ensureWebDirectories(cfg); err != nil {
		return err
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

	db, sysLoggerCleanup, err := initDatabase(cfg, sysLogger)
	if err != nil {
		return err
	}
	defer sysLoggerCleanup()

	srv, err := buildWebServer(ctx, cfg, db, sysLogger)
	if err != nil {
		return err
	}

	url := fmt.Sprintf("http://localhost:%d", cfg.Port)
	sysLogger.Info("server ready", "url", url)

	if !noBrowser {
		go openBrowser(url)
	}

	return srv.Run(ctx)
}

func ensureWebDirectories(cfg *config.AppConfig) error {
	for _, dir := range []string{cfg.DataDir, cfg.LogDir()} {
		if err := os.MkdirAll(dir, 0750); err != nil {
			return fmt.Errorf("creating directory %s: %w", dir, err)
		}
	}
	if err := os.MkdirAll(config.DefaultWorkingDir(), 0750); err != nil {
		return fmt.Errorf("creating default working directory: %w", err)
	}
	return nil
}

func initDatabase(cfg *config.AppConfig, sysLogger *slog.Logger) (*sql.DB, func(), error) {
	dbPath := filepath.Join(cfg.DataDir, "agento.db")
	db, freshDB, err := storage.NewSQLiteDB(dbPath)
	if err != nil {
		return nil, nil, fmt.Errorf("opening database: %w", err)
	}
	cleanup := func() {
		if cerr := db.Close(); cerr != nil {
			sysLogger.Error("failed to close database", "error", cerr)
		}
	}

	if freshDB && storage.HasFSData(cfg.DataDir) {
		sysLogger.Info("detected existing filesystem data, migrating to SQLite...")
		if migrateErr := storage.MigrateFromFS(db, cfg.DataDir, sysLogger); migrateErr != nil {
			cleanup()
			return nil, nil, fmt.Errorf("migrating filesystem data to SQLite: %w", migrateErr)
		}
	}

	return db, cleanup, nil
}

func buildWebServer(
	ctx context.Context, cfg *config.AppConfig,
	db *sql.DB, sysLogger *slog.Logger,
) (*server.Server, error) {
	agentStore := storage.NewSQLiteAgentStore(db)

	existing, err := agentStore.List()
	if err != nil {
		return nil, fmt.Errorf("listing agents: %w", err)
	}
	if len(existing) == 0 {
		if seedErr := seedExampleAgent(agentStore); seedErr != nil {
			sysLogger.Warn("could not seed example agent", "error", seedErr)
		}
	}

	mcpRegistry, err := config.LoadMCPRegistry(cfg.MCPsFile())
	if err != nil {
		return nil, fmt.Errorf("loading MCP registry: %w", err)
	}

	localToolsMCP, err := tools.StartLocalMCPServer(ctx)
	if err != nil {
		return nil, fmt.Errorf("starting local tools MCP server: %w", err)
	}

	chatStore := storage.NewSQLiteChatStore(db)
	integrationStore := storage.NewSQLiteIntegrationStore(db)
	integrationRegistry := integrations.NewRegistry(integrationStore, sysLogger)
	integrationRegistry.RegisterStarter("google", googleintegration.Start)
	if startErr := integrationRegistry.Start(ctx); startErr != nil {
		sysLogger.Warn("some integrations failed to start", "error", startErr)
	}

	settingsStore := storage.NewSQLiteSettingsStore(db)
	settingsMgr, err := config.NewSettingsManager(settingsStore, cfg)
	if err != nil {
		return nil, fmt.Errorf("initializing settings: %w", err)
	}

	apiSrv, bus, err := buildAPIServer(ctx, db, sysLogger,
		agentStore, chatStore, integrationStore, integrationRegistry,
		mcpRegistry, localToolsMCP, settingsMgr,
	)
	if err != nil {
		return nil, err
	}
	srv := server.New(apiSrv, WebFS, cfg.Port, sysLogger)

	// Ensure the event bus is drained cleanly on shutdown.
	go func() {
		<-ctx.Done()
		bus.Close()
	}()

	return srv, nil
}

// buildAPIServer wires all services and returns the api.Server and the event bus.
func buildAPIServer(
	ctx context.Context,
	db *sql.DB,
	sysLogger *slog.Logger,
	agentStore storage.AgentStore,
	chatStore storage.ChatStore,
	integrationStore storage.IntegrationStore,
	integrationRegistry *integrations.IntegrationRegistry,
	mcpRegistry *config.MCPRegistry,
	localToolsMCP *tools.LocalMCPConfig,
	settingsMgr *config.SettingsManager,
) (*api.Server, eventbus.EventBus, error) {
	notifStore, bus := setupNotifications(db, settingsMgr)

	agentSvc := service.NewAgentService(agentStore, sysLogger)
	chatSvc := service.NewChatService(
		chatStore, agentStore, mcpRegistry, localToolsMCP,
		integrationRegistry, settingsMgr, sysLogger,
	)
	integrationSvc := service.NewIntegrationService(integrationStore, integrationRegistry, sysLogger, ctx)
	notificationSvc := service.NewNotificationService(settingsMgr, notifStore)

	taskStore := storage.NewSQLiteTaskStore(db)
	taskSvc := service.NewTaskService(taskStore, sysLogger)

	taskScheduler, err := initTaskScheduler(ctx, taskStore, chatStore, agentStore,
		mcpRegistry, localToolsMCP, integrationRegistry, settingsMgr, sysLogger)
	if err != nil {
		return nil, nil, err
	}

	sessionCache := claudesessions.NewCache(db, sysLogger)
	sessionCache.StartBackgroundScan()

	apiSrv := api.New(
		agentSvc, chatSvc, integrationSvc, notificationSvc, taskSvc,
		settingsMgr, sysLogger, sessionCache, taskScheduler,
	)
	return apiSrv, bus, nil
}

// setupNotifications creates the notification store, event bus, and wires the
// notification handler as a subscriber. The bus is returned so the caller can
// close it on shutdown.
func setupNotifications(
	db *sql.DB,
	settingsMgr *config.SettingsManager,
) (storage.NotificationStore, eventbus.EventBus) {
	workerPoolSize := settingsMgr.Get().EventBusWorkerPoolSize
	if workerPoolSize <= 0 {
		workerPoolSize = 3
	}

	bus := eventbus.New(workerPoolSize)
	notifStore := storage.NewSQLiteNotificationStore(db)
	notifHandler := notification.NewNotificationHandler(
		func() (*notification.NotificationSettings, error) {
			us := settingsMgr.Get()
			return loadNotificationSettingsFromJSON(us.NotificationSettings)
		},
		notifStore,
	)
	bus.Subscribe(func(e eventbus.Event) {
		notifHandler.Handle(e.Type, e.Payload)
	})
	return notifStore, bus
}

// loadNotificationSettingsFromJSON parses the JSON-encoded notification settings stored
// in the user_settings row. It is passed as a SettingsLoader to NewNotificationHandler
// so that configuration changes take effect without a server restart.
func loadNotificationSettingsFromJSON(raw string) (*notification.NotificationSettings, error) {
	if raw == "" || raw == "{}" {
		return &notification.NotificationSettings{}, nil
	}
	var ns notification.NotificationSettings
	if err := json.Unmarshal([]byte(raw), &ns); err != nil {
		return nil, fmt.Errorf("parsing notification settings: %w", err)
	}
	return &ns, nil
}

func initTaskScheduler(
	ctx context.Context,
	taskStore storage.TaskStore, chatStore storage.ChatStore, agentStore storage.AgentStore,
	mcpRegistry *config.MCPRegistry, localMCP *tools.LocalMCPConfig,
	integrationRegistry *integrations.IntegrationRegistry,
	settingsMgr *config.SettingsManager, sysLogger *slog.Logger,
) (*scheduler.Scheduler, error) {
	taskScheduler, err := scheduler.New(scheduler.Config{
		TaskStore:           taskStore,
		ChatStore:           chatStore,
		AgentStore:          agentStore,
		MCPRegistry:         mcpRegistry,
		LocalMCP:            localMCP,
		IntegrationRegistry: integrationRegistry,
		SettingsManager:     settingsMgr,
		Logger:              sysLogger,
	})
	if err != nil {
		return nil, fmt.Errorf("creating task scheduler: %w", err)
	}
	if startErr := taskScheduler.Start(ctx); startErr != nil {
		sysLogger.Warn("failed to start task scheduler", "error", startErr)
	}
	return taskScheduler, nil
}

func seedExampleAgent(store storage.AgentStore) error {
	agent := &config.AgentConfig{
		Name:        "Hello World",
		Slug:        "hello-world",
		Description: "A friendly assistant to help you get started with Agento.",
		Model:       "",
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

// printBanner writes the startup banner to stdout. It is the only output
// visible in the terminal during normal operation; all structured logs go
// to the log file instead.
const (
	githubRepo  = "https://github.com/shaharia-lab/agento"
	description = "Your personal AI agent platform using Claude Code CLI"
)

func printBanner(version, serverURL, logFile string) {
	if termenv.ColorProfile() == termenv.Ascii {
		printPlainBanner(version, serverURL, logFile)
		return
	}
	printFancyBanner(version, serverURL, logFile)
}

func printFancyBanner(version, serverURL, logFile string) {
	logo := lipgloss.NewStyle().
		Bold(true).
		Foreground(lipgloss.Color("12")). // bright blue
		Render(`
 █████╗  ██████╗ ███████╗███╗   ██╗████████╗ ██████╗
██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝██╔═══██╗
███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║   ██║   ██║
██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║   ██║   ██║
██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║   ╚██████╔╝
╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝    ╚═════╝
`)

	desc := lipgloss.NewStyle().
		Foreground(lipgloss.Color("8")). // muted gray
		Italic(true).
		Render(description)

	keyStyle := lipgloss.NewStyle().
		Foreground(lipgloss.Color("8")). // dark gray
		Width(10)

	valStyle := lipgloss.NewStyle().
		Foreground(lipgloss.Color("15")) // bright white

	urlStyle := lipgloss.NewStyle().
		Foreground(lipgloss.Color("14")). // bright cyan
		Underline(true)

	borderStyle := lipgloss.NewStyle().
		Border(lipgloss.RoundedBorder()).
		BorderForeground(lipgloss.Color("8")).
		PaddingLeft(1).
		PaddingRight(2)

	rows := []string{
		keyStyle.Render("Version") + valStyle.Render(version),
		keyStyle.Render("URL") + urlStyle.Render(serverURL),
		keyStyle.Render("Logs") + valStyle.Render(logFile),
		keyStyle.Render("GitHub") + urlStyle.Render(githubRepo),
	}

	table := borderStyle.Render(lipgloss.JoinVertical(lipgloss.Left, rows...))

	fmt.Println(logo)
	fmt.Println(desc)
	fmt.Println()
	fmt.Println(table)
	fmt.Println()
}

func printPlainBanner(version, serverURL, logFile string) {
	fmt.Println("Agento")
	fmt.Println(description)
	fmt.Println()
	fmt.Printf("  Version  %s\n", version)
	fmt.Printf("  URL      %s\n", serverURL)
	fmt.Printf("  Logs     %s\n", logFile)
	fmt.Printf("  GitHub   %s\n", githubRepo)
	fmt.Println()
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
	if err := c.Start(); err != nil {
		log.Printf("failed to open browser: %v", err)
	}
}
