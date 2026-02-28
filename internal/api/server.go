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
	"github.com/shaharia-lab/agento/internal/service"
)

// Server holds all dependencies for the REST API handlers.
type Server struct {
	agentSvc           service.AgentService
	chatSvc            service.ChatService
	integrationSvc     service.IntegrationService
	settingsMgr        *config.SettingsManager
	logger             *slog.Logger
	liveSessions       *liveSessionStore
	claudeSessionCache *claudesessions.Cache
}

// New creates a new API Server backed by the provided services.
func New(
	agentSvc service.AgentService,
	chatSvc service.ChatService,
	integrationSvc service.IntegrationService,
	settingsMgr *config.SettingsManager,
	logger *slog.Logger,
	sessionCache *claudesessions.Cache,
) *Server {
	return &Server{
		agentSvc:           agentSvc,
		chatSvc:            chatSvc,
		integrationSvc:     integrationSvc,
		settingsMgr:        settingsMgr,
		logger:             logger,
		liveSessions:       newLiveSessionStore(),
		claudeSessionCache: sessionCache,
	}
}

// Mount registers all API routes under the given router.
func (s *Server) Mount(r chi.Router) {
	// Agents CRUD
	r.Get("/agents", s.handleListAgents)
	r.Post("/agents", s.handleCreateAgent)
	r.Get("/agents/{slug}", s.handleGetAgent)
	r.Put("/agents/{slug}", s.handleUpdateAgent)
	r.Delete("/agents/{slug}", s.handleDeleteAgent)

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
	r.Get("/claude-settings/profiles/{id}", s.handleGetClaudeSettingsProfile)
	r.Put("/claude-settings/profiles/{id}", s.handleUpdateClaudeSettingsProfile)
	r.Delete("/claude-settings/profiles/{id}", s.handleDeleteClaudeSettingsProfile)
	r.Post("/claude-settings/profiles/{id}/duplicate", s.handleDuplicateClaudeSettingsProfile)
	r.Put("/claude-settings/profiles/{id}/default", s.handleSetDefaultClaudeSettingsProfile)

	// Claude Code sessions (read from ~/.claude)
	r.Get("/claude-sessions", s.handleListClaudeSessions)
	r.Get("/claude-sessions/projects", s.handleListClaudeProjects)
	r.Post("/claude-sessions/refresh", s.handleRefreshClaudeSessionCache)
	r.Get("/claude-sessions/{id}", s.handleGetClaudeSession)
	r.Post("/claude-sessions/{id}/continue", s.handleContinueClaudeSession)

	// Claude Code analytics
	r.Get("/claude-analytics", s.handleGetClaudeAnalytics)

	// Filesystem browser
	r.Get("/fs", s.handleFSList)
	r.Post("/fs/mkdir", s.handleFSMkdir)

	// Integrations
	r.Get("/integrations/available-tools", s.handleAvailableTools)
	r.Get("/integrations", s.handleListIntegrations)
	r.Post("/integrations", s.handleCreateIntegration)
	r.Get("/integrations/{id}", s.handleGetIntegration)
	r.Put("/integrations/{id}", s.handleUpdateIntegration)
	r.Delete("/integrations/{id}", s.handleDeleteIntegration)
	r.Post("/integrations/{id}/auth/start", s.handleStartOAuth)
	r.Get("/integrations/{id}/auth/status", s.handleGetAuthStatus)

	// Build info
	r.Get("/version", s.handleVersion)
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
