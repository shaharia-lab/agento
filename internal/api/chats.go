package api

import (
	"encoding/json"
	"errors"
	"net/http"
	"unicode/utf8"

	"github.com/go-chi/chi/v5"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/agent"
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

	// inputCh receives the user's answer to an AskUserQuestion prompt.
	// questionCh carries the question input data from the PermissionHandler
	// (which runs in the SDK reader goroutine) to this HTTP handler goroutine
	// so we can write user_input_required to SSE without a data race.
	inputCh := make(chan string, 1)
	questionCh := make(chan json.RawMessage, 4)

	// PermissionHandler is called synchronously in the SDK reader goroutine
	// before a tool executes. For AskUserQuestion we block here — the claude
	// subprocess naturally pauses because it is waiting for our control_response.
	permHandler := claude.PermissionHandler(func(toolName string, input json.RawMessage, _ claude.PermissionContext) claude.PermissionResult {
		if toolName != "AskUserQuestion" {
			return claude.PermissionResult{Behavior: "allow"}
		}

		// Signal the HTTP handler goroutine to send user_input_required over SSE.
		select {
		case questionCh <- input:
		default:
		}

		// Block until the user submits their answer or the request is canceled.
		select {
		case answer := <-inputCh:
			// Return the answers as the tool-call "error" content so the agent
			// receives them and can process them in its next reasoning step.
			return claude.PermissionResult{Behavior: "deny", Message: answer}
		case <-r.Context().Done():
			return claude.PermissionResult{Behavior: "deny", Message: "request canceled"}
		}
	})

	agentSession, chatSession, err := s.chatSvc.BeginMessage(r.Context(), id, req.Content, agent.RunOptions{
		PermissionHandler: permHandler,
	})
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

	// Register the live session so handleProvideInput can inject answers.
	s.liveSessions.put(id, &liveSession{session: agentSession, inputCh: inputCh})
	defer func() {
		s.liveSessions.delete(id)
		_ = agentSession.Close()
		close(questionCh)
	}()

	var assistantText string
	var sdkSessionID string

	// Use select so we can interleave SDK events with user_input_required
	// SSE writes from the same goroutine, avoiding any race on ResponseWriter.
	eventsCh := agentSession.Events()
	for {
		select {
		case event, ok := <-eventsCh:
			if !ok {
				goto done
			}
			if len(event.Raw) > 0 {
				sendSSERaw(w, flusher, string(event.Type), event.Raw)
			}
			if event.Type == claude.TypeResult && event.Result != nil {
				sdkSessionID = event.Result.SessionID
				if event.Result.IsError {
					return
				}
				assistantText = event.Result.Result
			}

		case qInput := <-questionCh:
			// PermissionHandler is blocking the subprocess, waiting for the user.
			// Send the question over SSE then wait; the subprocess stays paused.
			sendSSEEvent(w, flusher, "user_input_required", map[string]any{
				"input": qInput,
			})

		case <-r.Context().Done():
			return
		}
	}

done:
	if isFirstMessage {
		title := req.Content
		if utf8.RuneCountInString(title) > 60 {
			runes := []rune(title)
			title = string(runes[:60]) + "..."
		}
		chatSession.Title = title
	}

	if err := s.chatSvc.CommitMessage(r.Context(), chatSession, assistantText, sdkSessionID, isFirstMessage); err != nil {
		s.logger.Error("commit message failed", "session_id", id, "error", err)
	}
}

// handleProvideInput injects the user's answer to an AskUserQuestion prompt.
// It unblocks the PermissionHandler which was pausing the subprocess.
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
