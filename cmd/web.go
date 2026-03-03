package cmd

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
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
	confluenceintegration "github.com/shaharia-lab/agento/internal/integrations/confluence"
	githubintegration "github.com/shaharia-lab/agento/internal/integrations/github"
	googleintegration "github.com/shaharia-lab/agento/internal/integrations/google"
	jiraintegration "github.com/shaharia-lab/agento/internal/integrations/jira"
	slackintegration "github.com/shaharia-lab/agento/internal/integrations/slack"
	telegramintegration "github.com/shaharia-lab/agento/internal/integrations/telegram"
	"github.com/shaharia-lab/agento/internal/logger"
	"github.com/shaharia-lab/agento/internal/notification"
	"github.com/shaharia-lab/agento/internal/scheduler"
	"github.com/shaharia-lab/agento/internal/server"
	"github.com/shaharia-lab/agento/internal/service"
	"github.com/shaharia-lab/agento/internal/storage"
	"github.com/shaharia-lab/agento/internal/telemetry"
	"github.com/shaharia-lab/agento/internal/tools"
)

// noopCleanup is a no-op cleanup function returned on early-exit error paths
// where no resources have been acquired yet.
var noopCleanup = func() {} //nolint:gochecknoglobals

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

			if err := runWeb(cfg, noBrowser); err != nil {
				fmt.Fprintf(os.Stderr, "Error: %v\n", err)
				fmt.Fprintf(os.Stderr, "Check logs at: %s\n", logFile)
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

	otelCfg, otelProviders, sysLogger, logCleanup, err := initObservability(ctx, cfg)
	if err != nil {
		return err
	}
	defer logCleanup()

	sysLogger.Info("agento starting",
		slog.Int("port", cfg.Port),
		slog.String("data_dir", cfg.DataDir),
		slog.String("version", build.Version),
		slog.String("commit", build.CommitSHA),
		slog.String("build_date", build.BuildDate),
	)

	db, dbCleanup, err := initDatabase(cfg, sysLogger)
	if err != nil {
		return err
	}
	defer dbCleanup()

	srv, monitoringMgr, err := buildWebServer(ctx, cfg, db, sysLogger, otelCfg, otelProviders)
	if err != nil {
		sysLogger.Error("startup failed", "error", err)
		return err
	}
	defer func() {
		shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer shutdownCancel()
		if shutdownErr := monitoringMgr.Shutdown(shutdownCtx); shutdownErr != nil {
			sysLogger.Error("telemetry shutdown error", "error", shutdownErr)
		}
	}()

	url := fmt.Sprintf("http://localhost:%d", cfg.Port)
	sysLogger.Info("server ready", "url", url)

	if !noBrowser {
		go openBrowser(url, sysLogger)
	}

	return srv.Run(ctx)
}

// initObservability initializes OpenTelemetry and the structured logger, then logs the
// telemetry mode. It returns the monitoring config, providers, logger, and a cleanup func.
func initObservability(
	ctx context.Context, cfg *config.AppConfig,
) (telemetry.MonitoringConfig, *telemetry.Providers, *slog.Logger, func(), error) {
	otelCfg := telemetry.ConfigFromEnv()
	otelProviders, err := telemetry.Init(ctx, otelCfg)
	if err != nil {
		return otelCfg, nil, nil, noopCleanup, fmt.Errorf("initializing telemetry: %w", err)
	}

	sysLogger, logCleanup, err := logger.NewSystemLogger(cfg.LogDir(), cfg.SlogLevel())
	if err != nil {
		// Shut down already-initialized OTel providers before returning.
		// Logger is unavailable here so use the default slog sink.
		shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer shutdownCancel()
		if shutdownErr := otelProviders.Shutdown(shutdownCtx); shutdownErr != nil {
			slog.Default().Error("telemetry shutdown during logger init failure", "error", shutdownErr)
		}
		return otelCfg, nil, nil, noopCleanup, fmt.Errorf("initializing logger: %w", err)
	}

	logTelemetryMode(sysLogger, otelCfg)

	return otelCfg, otelProviders, sysLogger, logCleanup, nil
}

// logTelemetryMode logs whether telemetry is enabled or disabled with the configured exporters.
func logTelemetryMode(logger *slog.Logger, cfg telemetry.MonitoringConfig) {
	if cfg.Enabled {
		logger.Info("telemetry enabled",
			"metrics_exporter", string(cfg.MetricsExporter),
			"logs_exporter", string(cfg.LogsExporter),
			"otlp_endpoint", cfg.OTLPEndpoint,
		)
		return
	}
	logger.Info("telemetry disabled (set OTEL_METRICS_EXPORTER or OTEL_EXPORTER_OTLP_ENDPOINT to enable)")
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
	dbPath := cfg.DatabasePath()
	db, freshDB, err := storage.NewSQLiteDB(dbPath, sysLogger)
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
	otelCfg telemetry.MonitoringConfig, otelProviders *telemetry.Providers,
) (*server.Server, *telemetry.MonitoringManager, error) {
	agentStore := storage.NewSQLiteAgentStore(db)

	mcpRegistry, err := config.LoadMCPRegistry(cfg.MCPsFile())
	if err != nil {
		return nil, nil, fmt.Errorf("loading MCP registry: %w", err)
	}

	localToolsMCP, err := tools.StartLocalMCPServer(ctx)
	if err != nil {
		return nil, nil, fmt.Errorf("starting local tools MCP server: %w", err)
	}

	chatStore := storage.NewSQLiteChatStore(db)
	integrationStore := storage.NewSQLiteIntegrationStore(db)
	integrationRegistry := integrations.NewRegistry(integrationStore, sysLogger)
	integrationRegistry.RegisterStarter("confluence", confluenceintegration.Start)
	integrationRegistry.RegisterStarter("google", googleintegration.Start)
	integrationRegistry.RegisterStarter("telegram", telegramintegration.Start)
	integrationRegistry.RegisterStarter("jira", jiraintegration.Start)
	integrationRegistry.RegisterStarter("github", githubintegration.Start)
	integrationRegistry.RegisterStarter("slack", slackintegration.Start)
	if startErr := integrationRegistry.Start(ctx); startErr != nil {
		sysLogger.Warn("some integrations failed to start", "error", startErr)
	}

	settingsStore := storage.NewSQLiteSettingsStore(db)
	settingsMgr, err := config.NewSettingsManager(settingsStore, cfg)
	if err != nil {
		return nil, nil, fmt.Errorf("initializing settings: %w", err)
	}

	monitoringMgr := initMonitoringManager(cfg.DataDir, otelProviders, otelCfg, sysLogger)

	apiSrv, bus, err := buildAPIServer(ctx, appDeps{
		db:                  db,
		logger:              sysLogger,
		agentStore:          agentStore,
		chatStore:           chatStore,
		integrationStore:    integrationStore,
		integrationRegistry: integrationRegistry,
		mcpRegistry:         mcpRegistry,
		localToolsMCP:       localToolsMCP,
		settingsMgr:         settingsMgr,
		monitoringMgr:       monitoringMgr,
	})
	if err != nil {
		return nil, nil, err
	}
	srv := server.New(apiSrv, WebFS, cfg.Port, sysLogger, monitoringMgr)

	// Ensure the event bus is drained cleanly on shutdown.
	go func() {
		<-ctx.Done()
		bus.Close()
	}()

	return srv, monitoringMgr, nil
}

// initMonitoringManager creates a MonitoringManager, loads any persisted config from
// disk, and logs a warning on load failure (non-fatal).
func initMonitoringManager(
	dataDir string, providers *telemetry.Providers,
	envCfg telemetry.MonitoringConfig, logger *slog.Logger,
) *telemetry.MonitoringManager {
	mgr := telemetry.NewMonitoringManager(dataDir, providers, envCfg)
	if err := mgr.Load(); err != nil {
		logger.Warn("failed to load persisted monitoring config", "error", err)
	}
	return mgr
}

// appDeps bundles the stores, registries, and configuration needed to wire up
// the API server and task scheduler. It replaces the long parameter lists of
// buildAPIServer and initTaskScheduler.
type appDeps struct {
	db                  *sql.DB
	logger              *slog.Logger
	agentStore          storage.AgentStore
	chatStore           storage.ChatStore
	integrationStore    storage.IntegrationStore
	integrationRegistry *integrations.IntegrationRegistry
	mcpRegistry         *config.MCPRegistry
	localToolsMCP       *tools.LocalMCPConfig
	settingsMgr         *config.SettingsManager
	monitoringMgr       *telemetry.MonitoringManager
}

// buildAPIServer wires all services and returns the api.Server and the event bus.
func buildAPIServer(ctx context.Context, deps appDeps) (*api.Server, eventbus.EventBus, error) {
	notifStore, bus := setupNotifications(deps.db, deps.settingsMgr, deps.logger)

	agentSvc := service.NewAgentService(deps.agentStore, deps.logger)
	chatSvc := service.NewChatService(
		deps.chatStore, deps.agentStore, deps.mcpRegistry, deps.localToolsMCP,
		deps.integrationRegistry, deps.settingsMgr, deps.logger,
	)
	integrationSvc := service.NewIntegrationService(deps.integrationStore, deps.integrationRegistry, deps.logger)
	notificationSvc := service.NewNotificationService(deps.settingsMgr, notifStore)

	taskStore := storage.NewSQLiteTaskStore(deps.db)

	taskScheduler, err := initTaskScheduler(ctx, deps, taskStore, bus)
	if err != nil {
		return nil, nil, err
	}

	taskSvc := service.NewTaskService(taskStore, taskScheduler, deps.logger)
	profileSvc := service.NewClaudeSettingsProfileService(deps.logger)

	sessionCache := claudesessions.NewCache(deps.db, deps.logger)
	sessionCache.StartBackgroundScan()

	apiSrv := api.New(api.ServerConfig{
		AgentSvc:        agentSvc,
		ChatSvc:         chatSvc,
		IntegrationSvc:  integrationSvc,
		NotificationSvc: notificationSvc,
		TaskSvc:         taskSvc,
		ProfileSvc:      profileSvc,
		SettingsMgr:     deps.settingsMgr,
		Logger:          deps.logger,
		SessionCache:    sessionCache,
		MonitoringMgr:   deps.monitoringMgr,
	})
	return apiSrv, bus, nil
}

// setupNotifications creates the notification store, event bus, and wires the
// notification handler as a subscriber. The bus is returned so the caller can
// close it on shutdown.
func setupNotifications(
	db *sql.DB,
	settingsMgr *config.SettingsManager,
	logger *slog.Logger,
) (storage.NotificationStore, eventbus.EventBus) {
	workerPoolSize := settingsMgr.Get().EventBusWorkerPoolSize
	if workerPoolSize <= 0 {
		workerPoolSize = 3
	}

	bus := eventbus.New(workerPoolSize, logger)
	notifStore := storage.NewSQLiteNotificationStore(db)
	notifHandler := notification.NewNotificationHandler(
		func() (*notification.NotificationSettings, error) {
			us := settingsMgr.Get()
			return loadNotificationSettingsFromJSON(us.NotificationSettings)
		},
		notifStore,
		logger,
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
	ctx context.Context, deps appDeps, taskStore storage.TaskStore,
	eventPublisher scheduler.EventPublisher,
) (*scheduler.Scheduler, error) {
	taskScheduler, err := scheduler.New(scheduler.Config{
		TaskStore:           taskStore,
		ChatStore:           deps.chatStore,
		AgentStore:          deps.agentStore,
		MCPRegistry:         deps.mcpRegistry,
		LocalMCP:            deps.localToolsMCP,
		IntegrationRegistry: deps.integrationRegistry,
		SettingsManager:     deps.settingsMgr,
		Logger:              deps.logger,
		EventPublisher:      eventPublisher,
	})
	if err != nil {
		return nil, fmt.Errorf("creating task scheduler: %w", err)
	}
	if startErr := taskScheduler.Start(ctx); startErr != nil {
		deps.logger.Warn("failed to start task scheduler", "error", startErr)
	}
	return taskScheduler, nil
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

func openBrowser(url string, logger *slog.Logger) {
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
		logger.Warn("failed to open browser", "error", err)
	}
}
