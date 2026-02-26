package api

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/service"
)

// Server holds all dependencies for the REST API handlers.
type Server struct {
	agentSvc     service.AgentService
	chatSvc      service.ChatService
	settingsMgr  *config.SettingsManager
	logger       *slog.Logger
	liveSessions *liveSessionStore
}

// New creates a new API Server backed by the provided services.
func New(agentSvc service.AgentService, chatSvc service.ChatService, settingsMgr *config.SettingsManager, logger *slog.Logger) *Server {
	return &Server{
		agentSvc:     agentSvc,
		chatSvc:      chatSvc,
		settingsMgr:  settingsMgr,
		logger:       logger,
		liveSessions: newLiveSessionStore(),
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

	// Agento settings
	r.Get("/settings", s.handleGetSettings)
	r.Put("/settings", s.handleUpdateSettings)

	// Claude Code settings (~/.claude/settings.json)
	r.Get("/claude-settings", s.handleGetClaudeSettings)
	r.Put("/claude-settings", s.handleUpdateClaudeSettings)

	// Filesystem browser
	r.Get("/fs", s.handleFSList)
	r.Post("/fs/mkdir", s.handleFSMkdir)
}

// ─── Shared helpers ───────────────────────────────────────────────────────────

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func writeError(w http.ResponseWriter, status int, msg string) {
	writeJSON(w, status, map[string]string{"error": msg})
}

func sendSSEEvent(w http.ResponseWriter, flusher http.Flusher, event string, data any) {
	b, _ := json.Marshal(data)
	_, _ = fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, string(b))
	if flusher != nil {
		flusher.Flush()
	}
}
