package api

import (
	"encoding/json"
	"errors"
	"net/http"
	"unicode/utf8"

	"github.com/go-chi/chi/v5"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/service"
)

type createChatRequest struct {
	// AgentSlug is optional. An empty value means no-agent (direct LLM) chat.
	AgentSlug string `json:"agent_slug"`
}

type sendMessageRequest struct {
	Content string `json:"content"`
}

func (s *Server) handleListChats(w http.ResponseWriter, r *http.Request) {
	sessions, err := s.chatSvc.ListSessions(r.Context())
	if err != nil {
		s.logger.Error("list chats failed", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to list chats")
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

	session, err := s.chatSvc.CreateSession(r.Context(), req.AgentSlug)
	if err != nil {
		var nfe *service.NotFoundError
		if errors.As(err, &nfe) {
			writeError(w, http.StatusNotFound, nfe.Error())
			return
		}
		s.logger.Error("create chat failed", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to create chat")
		return
	}
	writeJSON(w, http.StatusCreated, session)
}

func (s *Server) handleGetChat(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	session, messages, err := s.chatSvc.GetSessionWithMessages(r.Context(), id)
	if err != nil {
		s.logger.Error("get chat failed", "session_id", id, "error", err)
		writeError(w, http.StatusInternalServerError, "failed to get chat")
		return
	}
	if session == nil {
		writeError(w, http.StatusNotFound, "chat not found")
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"session":  session,
		"messages": messages,
	})
}

func (s *Server) handleDeleteChat(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := s.chatSvc.DeleteSession(r.Context(), id); err != nil {
		var nfe *service.NotFoundError
		if errors.As(err, &nfe) {
			writeError(w, http.StatusNotFound, nfe.Error())
			return
		}
		s.logger.Error("delete chat failed", "session_id", id, "error", err)
		writeError(w, http.StatusInternalServerError, "failed to delete chat")
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

	// BeginMessage validates the session, stores the user message, and starts streaming.
	stream, session, err := s.chatSvc.BeginMessage(r.Context(), id, req.Content)
	if err != nil {
		var nfe *service.NotFoundError
		if errors.As(err, &nfe) {
			writeError(w, http.StatusNotFound, nfe.Error())
			return
		}
		s.logger.Error("begin message failed", "session_id", id, "error", err)
		writeError(w, http.StatusInternalServerError, "failed to start message")
		return
	}

	isFirstMessage := session.Title == "New Chat"

	// Set SSE headers — from here on we can only send events, not JSON errors.
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

	// Update session title from first user message.
	if isFirstMessage {
		title := req.Content
		if utf8.RuneCountInString(title) > 60 {
			runes := []rune(title)
			title = string(runes[:60]) + "..."
		}
		session.Title = title
	}

	// Persist assistant response and update session metadata.
	if err := s.chatSvc.CommitMessage(r.Context(), session, assistantText, sdkSessionID, isFirstMessage); err != nil {
		s.logger.Error("commit message failed", "session_id", id, "error", err)
	}
}
