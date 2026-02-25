package api

import (
	"encoding/json"
	"fmt"
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agents-platform-cc-go/internal/config"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/storage"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/tools"
)

// Server holds all dependencies for the REST API handlers.
type Server struct {
	agents      storage.AgentStore
	chats       storage.ChatStore
	mcpRegistry *config.MCPRegistry
	localMCP    *tools.LocalMCPConfig
}

// New creates a new API Server.
func New(
	agents storage.AgentStore,
	chats storage.ChatStore,
	mcpRegistry *config.MCPRegistry,
	localMCP *tools.LocalMCPConfig,
) *Server {
	return &Server{
		agents:      agents,
		chats:       chats,
		mcpRegistry: mcpRegistry,
		localMCP:    localMCP,
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
	fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, string(b))
	if flusher != nil {
		flusher.Flush()
	}
}
