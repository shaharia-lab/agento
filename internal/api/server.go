package api

import (
	"encoding/json"
	"fmt"
	"log"
	"log/slog"
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/claudesessions"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/scheduler"
	"github.com/shaharia-lab/agento/internal/service"
)

// Common error message constants used across API handlers.
const (
	errInvalidJSONBody = "invalid JSON body"
)

// Route pattern constants to avoid duplication.
const (
	routeAgentBySlug     = "/agents/{slug}"
	routeIntegrationByID = "/integrations/{id}"
	routeProfileByID     = "/claude-settings/profiles/{id}"
	routeTaskByID        = "/tasks/{id}"
	routeJobHistoryByID  = "/job-history/{id}"
)

// Server holds all dependencies for the REST API handlers.
type Server struct {
	agentSvc           service.AgentService
	chatSvc            service.ChatService
	integrationSvc     service.IntegrationService
	taskSvc            service.TaskService
	settingsMgr        *config.SettingsManager
	logger             *slog.Logger
	liveSessions       *liveSessionStore
	claudeSessionCache *claudesessions.Cache
	scheduler          *scheduler.Scheduler
}

// New creates a new API Server backed by the provided services.
func New(
	agentSvc service.AgentService,
	chatSvc service.ChatService,
	integrationSvc service.IntegrationService,
	taskSvc service.TaskService,
	settingsMgr *config.SettingsManager,
	logger *slog.Logger,
	sessionCache *claudesessions.Cache,
	sched *scheduler.Scheduler,
) *Server {
	return &Server{
		agentSvc:           agentSvc,
		chatSvc:            chatSvc,
		integrationSvc:     integrationSvc,
		taskSvc:            taskSvc,
		settingsMgr:        settingsMgr,
		logger:             logger,
		liveSessions:       newLiveSessionStore(),
		claudeSessionCache: sessionCache,
		scheduler:          sched,
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
	r.Get("/chats", s.handleListChats)
	r.Post("/chats", s.handleCreateChat)
	r.Get("/chats/{id}", s.handleGetChat)
	r.Delete("/chats/{id}", s.handleDeleteChat)
	r.Post("/chats/{id}/messages", s.handleSendMessage)
	r.Post("/chats/{id}/input", s.handleProvideInput)
	r.Post("/chats/{id}/permission", s.handlePermissionResponse)

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

	// Claude Code sessions (read from ~/.claude)
	r.Get("/claude-sessions", s.handleListClaudeSessions)
	r.Get("/claude-sessions/projects", s.handleListClaudeProjects)
	r.Post("/claude-sessions/refresh", s.handleRefreshClaudeSessionCache)
	r.Get("/claude-sessions/{id}", s.handleGetClaudeSession)
	r.Post("/claude-sessions/{id}/continue", s.handleContinueClaudeSession)

	// Claude Code analytics
	r.Get("/claude-analytics", s.handleGetClaudeAnalytics)

	// Filesystem, integrations, tasks, job history
	s.mountExtensionRoutes(r)

	// Build info
	r.Get("/version", s.handleVersion)
}

// mountExtensionRoutes registers filesystem, integration, task, and job-history routes.
func (s *Server) mountExtensionRoutes(r chi.Router) {
	// Filesystem browser
	r.Get("/fs", s.handleFSList)
	r.Post("/fs/mkdir", s.handleFSMkdir)

	// Integrations
	r.Get("/integrations/available-tools", s.handleAvailableTools)
	r.Get("/integrations", s.handleListIntegrations)
	r.Post("/integrations", s.handleCreateIntegration)
	r.Get(routeIntegrationByID, s.handleGetIntegration)
	r.Put(routeIntegrationByID, s.handleUpdateIntegration)
	r.Delete(routeIntegrationByID, s.handleDeleteIntegration)
	r.Post(routeIntegrationByID+"/auth/start", s.handleStartOAuth)
	r.Get(routeIntegrationByID+"/auth/status", s.handleGetAuthStatus)

	// Scheduled tasks
	r.Get("/tasks", s.handleListTasks)
	r.Post("/tasks", s.handleCreateTask)
	r.Get(routeTaskByID, s.handleGetTask)
	r.Put(routeTaskByID, s.handleUpdateTask)
	r.Delete(routeTaskByID, s.handleDeleteTask)
	r.Post(routeTaskByID+"/pause", s.handlePauseTask)
	r.Post(routeTaskByID+"/resume", s.handleResumeTask)
	r.Get(routeTaskByID+"/job-history", s.handleListTaskJobHistory)
	r.Get("/job-history", s.handleListAllJobHistory)
	r.Get(routeJobHistoryByID, s.handleGetJobHistory)
}

// ─── Shared helpers ───────────────────────────────────────────────────────────

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	if err := json.NewEncoder(w).Encode(v); err != nil {
		log.Printf("writeJSON: failed to encode response: %v", err)
	}
}

func writeError(w http.ResponseWriter, status int, msg string) {
	writeJSON(w, status, map[string]string{"error": msg})
}

func sendSSEEvent(w http.ResponseWriter, flusher http.Flusher, event string, data any) {
	b, err := json.Marshal(data)
	if err != nil {
		log.Printf("sendSSEEvent: failed to marshal data: %v", err)
		return
	}
	if _, err := fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, string(b)); err != nil {
		return
	}
	if flusher != nil {
		flusher.Flush()
	}
}
