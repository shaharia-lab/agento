package server

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"time"

	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agents-platform-cc-go/internal/agent"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/config"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/tools"
)

// Server is the HTTP server for the agents platform.
type Server struct {
	agentRegistry *config.AgentRegistry
	mcpRegistry   *config.MCPRegistry
	localToolsMCP *tools.LocalMCPConfig
	port          int
	httpServer    *http.Server
}

// New creates a new Server.
func New(
	agentRegistry *config.AgentRegistry,
	mcpRegistry *config.MCPRegistry,
	localToolsMCP *tools.LocalMCPConfig,
	port int,
) *Server {
	s := &Server{
		agentRegistry: agentRegistry,
		mcpRegistry:   mcpRegistry,
		localToolsMCP: localToolsMCP,
		port:          port,
	}

	r := chi.NewRouter()
	r.Use(middleware.Recoverer)
	r.Use(middleware.RequestID)

	r.Get("/health", s.handleHealth)
	r.Get("/agents", s.handleListAgents)
	r.Post("/{slug}/ask", s.handleAsk)
	r.Post("/{slug}/ask/stream", s.handleAskStream)

	s.httpServer = &http.Server{
		Addr:    fmt.Sprintf(":%d", port),
		Handler: r,
	}

	return s
}

// Run starts the HTTP server and blocks until ctx is cancelled.
func (s *Server) Run(ctx context.Context) error {
	ln, err := net.Listen("tcp", s.httpServer.Addr)
	if err != nil {
		return fmt.Errorf("listening on %s: %w", s.httpServer.Addr, err)
	}

	errCh := make(chan error, 1)
	go func() {
		if err := s.httpServer.Serve(ln); err != nil && err != http.ErrServerClosed {
			errCh <- err
		}
	}()

	select {
	case <-ctx.Done():
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		return s.httpServer.Shutdown(shutdownCtx)
	case err := <-errCh:
		return err
	}
}

// ─── Handlers ────────────────────────────────────────────────────────────────

func (s *Server) handleHealth(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

func (s *Server) handleListAgents(w http.ResponseWriter, _ *http.Request) {
	agents := s.agentRegistry.List()
	result := make([]map[string]string, 0, len(agents))
	for _, a := range agents {
		result = append(result, map[string]string{
			"slug":        a.Slug,
			"name":        a.Name,
			"description": a.Description,
		})
	}
	writeJSON(w, http.StatusOK, result)
}

// askBody is the request body for the /ask and /ask/stream endpoints.
type askBody struct {
	Question  string            `json:"question"`
	SessionID string            `json:"session_id"`
	Thinking  *bool             `json:"thinking"`
	Variables map[string]string `json:"variables"`
}

func (s *Server) handleAsk(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	agentCfg := s.agentRegistry.Get(slug)
	if agentCfg == nil {
		writeJSON(w, http.StatusNotFound, map[string]string{"error": fmt.Sprintf("agent %q not found", slug)})
		return
	}

	var body askBody
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "invalid JSON body"})
		return
	}

	question := trimSpace(body.Question)
	if question == "" {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "missing required field: question"})
		return
	}

	noThinking := resolveNoThinking(body.Thinking, agentCfg)

	result, err := agent.RunAgent(r.Context(), agentCfg, question, agent.RunOptions{
		SessionID:     body.SessionID,
		NoThinking:    noThinking,
		Variables:     body.Variables,
		LocalToolsMCP: s.localToolsMCP,
		MCPRegistry:   s.mcpRegistry,
	})
	if err != nil {
		if mv, ok := err.(*agent.MissingVariableError); ok {
			writeJSON(w, http.StatusBadRequest, map[string]string{"error": mv.Error()})
			return
		}
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": err.Error()})
		return
	}

	writeJSON(w, http.StatusOK, map[string]any{
		"session_id": result.SessionID,
		"question":   question,
		"answer":     result.Answer,
		"thinking":   result.Thinking,
		"cost_usd":   result.CostUSD,
		"usage": map[string]int{
			"input_tokens":                result.Usage.InputTokens,
			"output_tokens":               result.Usage.OutputTokens,
			"cache_read_input_tokens":     result.Usage.CacheReadInputTokens,
			"cache_creation_input_tokens": result.Usage.CacheCreationInputTokens,
		},
	})
}

func (s *Server) handleAskStream(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	agentCfg := s.agentRegistry.Get(slug)
	if agentCfg == nil {
		writeJSON(w, http.StatusNotFound, map[string]string{"error": fmt.Sprintf("agent %q not found", slug)})
		return
	}

	var body askBody
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "invalid JSON body"})
		return
	}

	question := trimSpace(body.Question)
	if question == "" {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "missing required field: question"})
		return
	}

	noThinking := resolveNoThinking(body.Thinking, agentCfg)

	stream, err := agent.StreamAgent(r.Context(), agentCfg, question, agent.RunOptions{
		SessionID:     body.SessionID,
		NoThinking:    noThinking,
		Variables:     body.Variables,
		LocalToolsMCP: s.localToolsMCP,
		MCPRegistry:   s.mcpRegistry,
	})
	if err != nil {
		if mv, ok := err.(*agent.MissingVariableError); ok {
			writeJSON(w, http.StatusBadRequest, map[string]string{"error": mv.Error()})
			return
		}
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": err.Error()})
		return
	}

	// Set SSE headers
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)

	flusher, ok := w.(http.Flusher)
	if !ok {
		sendSSEEvent(w, flusher, "error", map[string]string{"error": "streaming not supported"})
		return
	}

	var sessionID string
	var streamedThinking string
	var finalThinking string

	defer func() {
		sendSSEEvent(w, flusher, "done", map[string]any{})
	}()

	for event := range stream.Events() {
		switch event.Type {
		case claude.TypeStreamEvent:
			if event.StreamEvent != nil {
				delta := event.StreamEvent.Event.Delta
				if delta != nil && delta.Type == "thinking_delta" && delta.Thinking != "" {
					streamedThinking += delta.Thinking
					sendSSEEvent(w, flusher, "thinking", map[string]string{"text": delta.Thinking})
				}
			}

		case claude.TypeAssistant:
			if event.Assistant != nil {
				text := event.Assistant.Text()
				if text != "" {
					sendSSEEvent(w, flusher, "message", map[string]string{"text": text})
				}
				if t := event.Assistant.Thinking(); t != "" {
					finalThinking = t
				}
			}

		case claude.TypeResult:
			if event.Result != nil {
				if sessionID == "" {
					sessionID = event.Result.SessionID
				}

				thinking := finalThinking
				if thinking == "" {
					thinking = streamedThinking
				}

				if event.Result.IsError {
					sendSSEEvent(w, flusher, "error", map[string]string{"error": event.Result.Result})
					return
				}

				sendSSEEvent(w, flusher, "result", map[string]any{
					"session_id": event.Result.SessionID,
					"answer":     event.Result.Result,
					"thinking":   thinking,
					"cost_usd":   event.Result.TotalCostUSD,
					"usage": map[string]int{
						"input_tokens":                event.Result.Usage.InputTokens,
						"output_tokens":               event.Result.Usage.OutputTokens,
						"cache_read_input_tokens":     event.Result.Usage.CacheReadInputTokens,
						"cache_creation_input_tokens": event.Result.Usage.CacheCreationInputTokens,
					},
				})
			}
		}
	}
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func sendSSEEvent(w http.ResponseWriter, flusher http.Flusher, event string, data any) {
	b, _ := json.Marshal(data)
	fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, string(b))
	if flusher != nil {
		flusher.Flush()
	}
}

func trimSpace(s string) string {
	result := ""
	for i, r := range s {
		if r != ' ' && r != '\t' && r != '\n' && r != '\r' {
			result = s[i:]
			break
		}
	}
	for i := len(result) - 1; i >= 0; i-- {
		r := result[i]
		if r != ' ' && r != '\t' && r != '\n' && r != '\r' {
			return result[:i+1]
		}
	}
	return result
}

// resolveNoThinking determines whether to disable thinking for this request.
// Request-level thinking flag overrides agent config.
func resolveNoThinking(thinkingFlag *bool, agentCfg *config.AgentConfig) bool {
	if thinkingFlag != nil && !*thinkingFlag {
		return true // caller explicitly disabled thinking
	}
	if agentCfg != nil && agentCfg.Thinking == "disabled" {
		return true
	}
	return false
}
