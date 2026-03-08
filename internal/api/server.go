package api

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/claudesessions"
	"github.com/shaharia-lab/agento/internal/config"
	whatsappintegration "github.com/shaharia-lab/agento/internal/integrations/whatsapp"
	"github.com/shaharia-lab/agento/internal/service"
	"github.com/shaharia-lab/agento/internal/telemetry"
)

// Common error message constants used across API handlers.
const (
	errInvalidJSONBody = "invalid JSON body"
	headerContentType  = "Content-Type"
)

// Route pattern constants to avoid duplication.
const (
	routeChatsBase       = "/chats"
	routeChatByID        = "/chats/{id}"
	routeAgentBySlug     = "/agents/{slug}"
	routeIntegrationByID = "/integrations/{id}"
	routeProfileByID     = "/claude-settings/profiles/{id}"
	routeTaskByID        = "/tasks/{id}"
	routeJobHistoryBase  = "/job-history"
	routeJobHistoryByID  = routeJobHistoryBase + "/{id}"
)

// ServerConfig bundles all dependencies needed to construct an API Server.
type ServerConfig struct {
	AgentSvc           service.AgentService
	ChatSvc            service.ChatService
	IntegrationSvc     service.IntegrationService
	NotificationSvc    service.NotificationService
	TaskSvc            service.TaskService
	TriggerSvc         service.TriggerService
	ProfileSvc         service.ClaudeSettingsProfileService
	SettingsMgr        *config.SettingsManager
	AppConfig          *config.AppConfig
	Logger             *slog.Logger
	SessionCache       *claudesessions.Cache
	MonitoringMgr      *telemetry.MonitoringManager
	InsightStore       claudesessions.InsightStorer
	WhatsAppPairingMgr *whatsappintegration.PairingManager
}

// Server holds all dependencies for the REST API handlers.
type Server struct {
	agentSvc           service.AgentService
	chatSvc            service.ChatService
	integrationSvc     service.IntegrationService
	notificationSvc    service.NotificationService
	taskSvc            service.TaskService
	triggerSvc         service.TriggerService
	profileSvc         service.ClaudeSettingsProfileService
	settingsMgr        *config.SettingsManager
	appConfig          *config.AppConfig
	logger             *slog.Logger
	liveSessions       *liveSessionStore
	claudeSessionCache *claudesessions.Cache
	updateCache        updateCheckCache
	monitoringMgr      *telemetry.MonitoringManager
	insightStore       claudesessions.InsightStorer
	whatsappPairingMgr *whatsappintegration.PairingManager
}

// New creates a new API Server backed by the provided services.
func New(cfg ServerConfig) *Server {
	if cfg.Logger == nil {
		cfg.Logger = slog.Default()
	}
	return &Server{
		agentSvc:           cfg.AgentSvc,
		chatSvc:            cfg.ChatSvc,
		integrationSvc:     cfg.IntegrationSvc,
		notificationSvc:    cfg.NotificationSvc,
		taskSvc:            cfg.TaskSvc,
		triggerSvc:         cfg.TriggerSvc,
		profileSvc:         cfg.ProfileSvc,
		settingsMgr:        cfg.SettingsMgr,
		appConfig:          cfg.AppConfig,
		logger:             cfg.Logger,
		liveSessions:       newLiveSessionStore(),
		claudeSessionCache: cfg.SessionCache,
		monitoringMgr:      cfg.MonitoringMgr,
		insightStore:       cfg.InsightStore,
		whatsappPairingMgr: cfg.WhatsAppPairingMgr,
	}
}

// Mount registers all API routes under the given router.
func (s *Server) Mount(r chi.Router) {
	// Agents CRUD
	r.Get("/agents", s.handleListAgents)
	r.Post("/agents", s.handleCreateAgent)
	r.Get(routeAgentBySlug, s.handleGetAgent)
	r.Put(routeAgentBySlug, s.handleUpdateAgent)
	r.Delete(routeAgentBySlug, s.handleDeleteAgent)

	// Chat sessions
	r.Get(routeChatsBase, s.handleListChats)
	r.Post(routeChatsBase, s.handleCreateChat)
	r.Delete(routeChatsBase, s.handleBulkDeleteChats)
	r.Get(routeChatByID, s.handleGetChat)
	r.Patch(routeChatByID, s.handleUpdateChat)
	r.Delete(routeChatByID, s.handleDeleteChat)
	r.Post(routeChatByID+"/messages", s.handleSendMessage)
	r.Post(routeChatByID+"/input", s.handleProvideInput)
	r.Post(routeChatByID+"/permission", s.handlePermissionResponse)
	r.Post(routeChatByID+"/stop", s.handleStopSession)

	// Agento settings
	r.Get("/settings", s.handleGetSettings)
	r.Put("/settings", s.handleUpdateSettings)

	// Claude Code settings (~/.claude/settings.json)
	r.Get("/claude-settings", s.handleGetClaudeSettings)
	r.Put("/claude-settings", s.handleUpdateClaudeSettings)

	// Claude settings profiles
	r.Get("/claude-settings/profiles", s.handleListClaudeSettingsProfiles)
	r.Post("/claude-settings/profiles", s.handleCreateClaudeSettingsProfile)
	r.Get(routeProfileByID, s.handleGetClaudeSettingsProfile)
	r.Put(routeProfileByID, s.handleUpdateClaudeSettingsProfile)
	r.Delete(routeProfileByID, s.handleDeleteClaudeSettingsProfile)
	r.Post(routeProfileByID+"/duplicate", s.handleDuplicateClaudeSettingsProfile)
	r.Put(routeProfileByID+"/default", s.handleSetDefaultClaudeSettingsProfile)

	// Claude Code sessions and analytics
	s.mountClaudeSessionRoutes(r)

	// File uploads
	r.Post("/uploads", s.handleUploadFile)

	// Filesystem, integrations, tasks, job history
	s.mountExtensionRoutes(r)

	// Build info and update check
	r.Get("/version", s.handleVersion)
	r.Get("/version/update-check", s.handleUpdateCheck)

	// Monitoring / OTel configuration
	r.Get("/monitoring", s.getMonitoring)
	r.Put("/monitoring", s.putMonitoring)
	r.Post("/monitoring/test", s.testMonitoring)
}

// mountExtensionRoutes registers filesystem, integration, task, and job-history routes.
func (s *Server) mountExtensionRoutes(r chi.Router) {
	// Filesystem browser
	r.Get("/fs", s.handleFSList)
	r.Post("/fs/mkdir", s.handleFSMkdir)

	// Integrations
	s.mountIntegrationRoutes(r)

	// Notifications
	s.mountNotificationRoutes(r)

	// Scheduled tasks and job history
	s.mountTaskRoutes(r)

}

// mountClaudeSessionRoutes registers Claude Code session and analytics routes.
func (s *Server) mountClaudeSessionRoutes(r chi.Router) {
	r.Get("/claude-sessions", s.handleListClaudeSessions)
	r.Get("/claude-sessions/projects", s.handleListClaudeProjects)
	r.Post("/claude-sessions/refresh", s.handleRefreshClaudeSessionCache)
	// Insights summary must come before /{id} to avoid chi routing conflicts.
	r.Get("/claude-sessions/insights/summary", s.handleGetClaudeSessionInsightsSummary)
	r.Get("/claude-sessions/{id}", s.handleGetClaudeSession)
	r.Patch("/claude-sessions/{id}", s.handleUpdateClaudeSession)
	r.Post("/claude-sessions/{id}/continue", s.handleContinueClaudeSession)
	r.Get("/claude-sessions/{id}/insights", s.handleGetClaudeSessionInsights)
	r.Get("/claude-sessions/{id}/journey", s.handleGetClaudeSessionJourney)
	r.Get("/claude-analytics", s.handleGetClaudeAnalytics)
}

// mountIntegrationRoutes registers integration-related routes.
func (s *Server) mountIntegrationRoutes(r chi.Router) {
	r.Get("/integrations/available-tools", s.handleAvailableTools)
	r.Get("/integrations", s.handleListIntegrations)
	r.Post("/integrations", s.handleCreateIntegration)
	r.Get(routeIntegrationByID, s.handleGetIntegration)
	r.Put(routeIntegrationByID, s.handleUpdateIntegration)
	r.Delete(routeIntegrationByID, s.handleDeleteIntegration)
	r.Post(routeIntegrationByID+"/auth/start", s.handleStartOAuth)
	r.Get(routeIntegrationByID+"/auth/status", s.handleGetAuthStatus)
	r.Post(routeIntegrationByID+"/auth/validate", s.handleValidateAuth)

	// WhatsApp QR code pairing
	r.Post(routeIntegrationByID+"/whatsapp/pair", s.handleStartWhatsAppPairing)
	r.Get(routeIntegrationByID+"/whatsapp/qr", s.handleGetWhatsAppQR)
	r.Get(routeIntegrationByID+"/whatsapp/status", s.handleGetWhatsAppStatus)
	r.Post(routeIntegrationByID+"/whatsapp/reconnect", s.handleWhatsAppReconnect)

	// Trigger rules CRUD
	r.Get(routeIntegrationByID+"/triggers", s.handleListTriggerRules)
	r.Post(routeIntegrationByID+"/triggers", s.handleCreateTriggerRule)
	r.Put(routeIntegrationByID+"/triggers/{rid}", s.handleUpdateTriggerRule)
	r.Delete(routeIntegrationByID+"/triggers/{rid}", s.handleDeleteTriggerRule)

	// Webhook management
	r.Post(routeIntegrationByID+"/webhook/register", s.handleRegisterWebhook)
	r.Delete(routeIntegrationByID+"/webhook/register", s.handleDeleteWebhook)
	r.Get(routeIntegrationByID+"/webhook/status", s.handleGetWebhookStatus)
	r.Post(routeIntegrationByID+"/webhook/regenerate-secret", s.handleRegenerateWebhookSecret)
}

// mountNotificationRoutes registers the notification-related API routes.
func (s *Server) mountNotificationRoutes(r chi.Router) {
	r.Get("/notifications/settings", s.handleGetNotificationSettings)
	r.Put("/notifications/settings", s.handleUpdateNotificationSettings)
	r.Post("/notifications/test", s.handleTestNotification)
	r.Get("/notifications/log", s.handleListNotificationLog)
}

// mountTaskRoutes registers scheduled task and job history routes.
func (s *Server) mountTaskRoutes(r chi.Router) {
	r.Get("/tasks", s.handleListTasks)
	r.Post("/tasks", s.handleCreateTask)
	r.Get(routeTaskByID, s.handleGetTask)
	r.Put(routeTaskByID, s.handleUpdateTask)
	r.Delete(routeTaskByID, s.handleDeleteTask)
	r.Post(routeTaskByID+"/pause", s.handlePauseTask)
	r.Post(routeTaskByID+"/resume", s.handleResumeTask)
	r.Get(routeTaskByID+routeJobHistoryBase, s.handleListTaskJobHistory)
	r.Get(routeJobHistoryBase, s.handleListAllJobHistory)
	r.Delete(routeJobHistoryBase, s.handleBulkDeleteJobHistory)
	r.Get(routeJobHistoryByID, s.handleGetJobHistory)
	r.Delete(routeJobHistoryByID, s.handleDeleteJobHistory)
}

// ─── Shared helpers ───────────────────────────────────────────────────────────

func (s *Server) writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set(headerContentType, "application/json")
	w.WriteHeader(status)
	if err := json.NewEncoder(w).Encode(v); err != nil {
		s.logger.Error("writeJSON: failed to encode response", "error", err)
	}
}

func (s *Server) writeError(w http.ResponseWriter, status int, msg string) {
	s.writeJSON(w, status, map[string]string{"error": msg})
}

func (s *Server) sendSSEEvent(w http.ResponseWriter, flusher http.Flusher, event string, data any) {
	b, err := json.Marshal(data)
	if err != nil {
		s.logger.Error("sendSSEEvent: failed to marshal data", "error", err)
		return
	}
	if _, err := fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, string(b)); err != nil {
		return
	}
	if flusher != nil {
		flusher.Flush()
	}
}
