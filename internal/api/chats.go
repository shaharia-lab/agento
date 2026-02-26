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

// sendSSERaw writes a raw JSON payload as an SSE event without re-marshaling.
func sendSSERaw(w http.ResponseWriter, flusher http.Flusher, event string, raw json.RawMessage) {
	_, _ = w.Write([]byte("event: " + event + "\ndata: "))
	_, _ = w.Write(raw)
	_, _ = w.Write([]byte("\n\n"))
	if flusher != nil {
		flusher.Flush()
	}
}

type createChatRequest struct {
	// AgentSlug is optional. An empty value means no-agent (direct LLM) chat.
	AgentSlug        string `json:"agent_slug"`
	WorkingDirectory string `json:"working_directory"`
	Model            string `json:"model"`
}

type sendMessageRequest struct {
	Content string `json:"content"`
}

type provideInputRequest struct {
	Answer string `json:"answer"`
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

	session, err := s.chatSvc.CreateSession(r.Context(), req.AgentSlug, req.WorkingDirectory, req.Model)
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

	// BeginMessage validates the session, stores the user message, and starts
	// a persistent SDK session with the first message already sent.
	agentSession, chatSession, err := s.chatSvc.BeginMessage(r.Context(), id, req.Content)
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

	isFirstMessage := chatSession.Title == "New Chat"

	// Set SSE headers — from here on we can only send events, not JSON errors.
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)

	flusher, ok := w.(http.Flusher)
	if !ok {
		sendSSEEvent(w, nil, "error", map[string]string{"error": "streaming not supported"})
		_ = agentSession.Close()
		return
	}

	// Register the live session so handleProvideInput can inject user answers.
	inputCh := make(chan string, 1)
	s.liveSessions.put(id, &liveSession{session: agentSession, inputCh: inputCh})
	defer func() {
		s.liveSessions.delete(id)
		_ = agentSession.Close()
	}()

	var assistantText string
	var sdkSessionID string
	// pendingInput holds the AskUserQuestion tool input we detected mid-turn;
	// non-nil means we're waiting for the user to respond.
	var pendingInput json.RawMessage

	for event := range agentSession.Events() {
		// Forward every raw SDK event to the frontend as-is.
		if len(event.Raw) > 0 {
			sendSSERaw(w, flusher, string(event.Type), event.Raw)
		}

		switch event.Type {
		case claude.TypeAssistant:
			// Detect AskUserQuestion tool_use in this turn's content blocks.
			if input := extractAskUserQuestionInput(event.Raw); input != nil {
				pendingInput = input
			}

		case claude.TypeResult:
			if event.Result == nil {
				continue
			}
			sdkSessionID = event.Result.SessionID
			if event.Result.IsError {
				return
			}
			assistantText = event.Result.Result

			if pendingInput != nil {
				// Agent called AskUserQuestion — tell the frontend and wait.
				sendSSEEvent(w, flusher, "user_input_required", map[string]any{
					"input": pendingInput,
				})
				pendingInput = nil

				select {
				case answer := <-inputCh:
					// Inject the user's answer as a new turn on the same subprocess.
					if err := agentSession.Send(answer); err != nil {
						s.logger.Error("inject answer failed", "session_id", id, "error", err)
						return
					}
					// Reset accumulated assistant text for the next turn.
					assistantText = ""
					// Continue ranging — more events follow from the new turn.
				case <-r.Context().Done():
					return
				}
			} else {
				// No pending question: this is the final result, we're done.
				// The defer will drain remaining events and close the session.
				goto done
			}
		}
	}

done:
	// Update session title from first user message.
	if isFirstMessage {
		title := req.Content
		if utf8.RuneCountInString(title) > 60 {
			runes := []rune(title)
			title = string(runes[:60]) + "..."
		}
		chatSession.Title = title
	}

	// Persist assistant response and update session metadata.
	if err := s.chatSvc.CommitMessage(r.Context(), chatSession, assistantText, sdkSessionID, isFirstMessage); err != nil {
		s.logger.Error("commit message failed", "session_id", id, "error", err)
	}
}

// handleProvideInput receives the user's answer to an AskUserQuestion prompt
// and injects it into the waiting SSE handler via the live session's inputCh.
func (s *Server) handleProvideInput(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	var req provideInputRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "invalid JSON body")
		return
	}
	if req.Answer == "" {
		writeError(w, http.StatusBadRequest, "answer is required")
		return
	}

	ls, ok := s.liveSessions.get(id)
	if !ok {
		writeError(w, http.StatusConflict, "no active session awaiting input for this chat")
		return
	}

	select {
	case ls.inputCh <- req.Answer:
		w.WriteHeader(http.StatusNoContent)
	default:
		writeError(w, http.StatusConflict, "session is not currently awaiting input")
	}
}

// extractAskUserQuestionInput parses a raw assistant event JSON and returns
// the input field of the first AskUserQuestion tool_use content block, or nil.
func extractAskUserQuestionInput(raw json.RawMessage) json.RawMessage {
	var msg struct {
		Message struct {
			Content []struct {
				Type  string          `json:"type"`
				Name  string          `json:"name,omitempty"`
				Input json.RawMessage `json:"input,omitempty"`
			} `json:"content"`
		} `json:"message"`
	}
	if err := json.Unmarshal(raw, &msg); err != nil {
		return nil
	}
	for _, block := range msg.Message.Content {
		if block.Type == "tool_use" && block.Name == "AskUserQuestion" {
			return block.Input
		}
	}
	return nil
}
