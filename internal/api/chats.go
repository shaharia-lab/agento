package api

import (
	"encoding/json"
	"fmt"
	"net/http"
	"time"

	"github.com/go-chi/chi/v5"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agents-platform-cc-go/internal/agent"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/storage"
)

type createChatRequest struct {
	AgentSlug string `json:"agent_slug"`
}

type sendMessageRequest struct {
	Content string `json:"content"`
}

func (s *Server) handleListChats(w http.ResponseWriter, r *http.Request) {
	sessions, err := s.chats.ListSessions()
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, sessions)
}

func (s *Server) handleCreateChat(w http.ResponseWriter, r *http.Request) {
	var req createChatRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "invalid JSON body")
		return
	}
	if req.AgentSlug == "" {
		writeError(w, http.StatusBadRequest, "agent_slug is required")
		return
	}

	agentCfg, err := s.agents.Get(req.AgentSlug)
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	if agentCfg == nil {
		writeError(w, http.StatusNotFound, fmt.Sprintf("agent %q not found", req.AgentSlug))
		return
	}

	session, err := s.chats.CreateSession(req.AgentSlug)
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusCreated, session)
}

func (s *Server) handleGetChat(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	session, messages, err := s.chats.GetSessionWithMessages(id)
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	if session == nil {
		writeError(w, http.StatusNotFound, fmt.Sprintf("chat %q not found", id))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"session":  session,
		"messages": messages,
	})
}

func (s *Server) handleDeleteChat(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := s.chats.DeleteSession(id); err != nil {
		writeError(w, http.StatusNotFound, err.Error())
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleSendMessage(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	var req sendMessageRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "invalid JSON body")
		return
	}
	if req.Content == "" {
		writeError(w, http.StatusBadRequest, "content is required")
		return
	}

	session, err := s.chats.GetSession(id)
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	if session == nil {
		writeError(w, http.StatusNotFound, fmt.Sprintf("chat %q not found", id))
		return
	}

	agentCfg, err := s.agents.Get(session.AgentSlug)
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	if agentCfg == nil {
		writeError(w, http.StatusNotFound, fmt.Sprintf("agent %q not found", session.AgentSlug))
		return
	}

	// Store user message before streaming
	userMsg := storage.ChatMessage{
		Role:      "user",
		Content:   req.Content,
		Timestamp: time.Now().UTC(),
	}
	if err := s.chats.AppendMessage(id, userMsg); err != nil {
		writeError(w, http.StatusInternalServerError, "failed to store message: "+err.Error())
		return
	}

	// Start streaming — do this before setting SSE headers so we can return a
	// proper error if the agent fails to start.
	stream, err := agent.StreamAgent(r.Context(), agentCfg, req.Content, agent.RunOptions{
		SessionID:     session.SDKSession,
		LocalToolsMCP: s.localMCP,
		MCPRegistry:   s.mcpRegistry,
	})
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to start agent: "+err.Error())
		return
	}

	// Update session title from first user message
	isFirstMessage := session.Title == "New Chat"

	// Set SSE headers — from here on we can only send events, not JSON errors
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)

	flusher, ok := w.(http.Flusher)
	if !ok {
		sendSSEEvent(w, nil, "error", map[string]string{"error": "streaming not supported"})
		return
	}

	var assistantText string
	var sdkSessionID string

	for event := range stream.Events() {
		switch event.Type {
		case claude.TypeStreamEvent:
			if event.StreamEvent != nil {
				delta := event.StreamEvent.Event.Delta
				if delta != nil {
					if delta.Type == "thinking_delta" && delta.Thinking != "" {
						sendSSEEvent(w, flusher, "thinking", map[string]string{"text": delta.Thinking})
					} else if delta.Type == "text_delta" && delta.Text != "" {
						sendSSEEvent(w, flusher, "text", map[string]string{"delta": delta.Text})
					}
				}
			}

		case claude.TypeResult:
			if event.Result != nil {
				sdkSessionID = event.Result.SessionID
				if event.Result.IsError {
					sendSSEEvent(w, flusher, "error", map[string]string{"error": event.Result.Result})
					return
				}
				assistantText = event.Result.Result
				sendSSEEvent(w, flusher, "done", map[string]any{
					"sdk_session_id": event.Result.SessionID,
					"cost_usd":       event.Result.TotalCostUSD,
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

	// Persist assistant message
	if assistantText != "" {
		assistantMsg := storage.ChatMessage{
			Role:      "assistant",
			Content:   assistantText,
			Timestamp: time.Now().UTC(),
		}
		_ = s.chats.AppendMessage(id, assistantMsg)
	}

	// Update session metadata
	session.SDKSession = sdkSessionID
	session.UpdatedAt = time.Now().UTC()
	if isFirstMessage {
		title := req.Content
		if len([]rune(title)) > 60 {
			title = string([]rune(title)[:60]) + "..."
		}
		session.Title = title
	}
	_ = s.chats.UpdateSession(session)
}
