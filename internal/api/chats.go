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
	"github.com/shaharia-lab/agento/internal/storage"
)

// tokenAccumulator accumulates token usage across multiple TypeResult events (multi-turn).
type tokenAccumulator struct {
	InputTokens              int
	OutputTokens             int
	CacheCreationInputTokens int
	CacheReadInputTokens     int
}

func (t *tokenAccumulator) add(r *claude.Result) {
	if r == nil {
		return
	}
	t.InputTokens += r.Usage.InputTokens
	t.OutputTokens += r.Usage.OutputTokens
	t.CacheCreationInputTokens += r.Usage.CacheCreationInputTokens
	t.CacheReadInputTokens += r.Usage.CacheReadInputTokens
}

func (t *tokenAccumulator) toUsageStats() agent.UsageStats {
	return agent.UsageStats{
		InputTokens:              t.InputTokens,
		OutputTokens:             t.OutputTokens,
		CacheCreationInputTokens: t.CacheCreationInputTokens,
		CacheReadInputTokens:     t.CacheReadInputTokens,
	}
}

// assistantEventRaw is used to parse content blocks out of a raw "assistant" SSE event.
type assistantEventRaw struct {
	Message struct {
		Content []struct {
			Type     string          `json:"type"`
			Text     string          `json:"text,omitempty"`
			Thinking string          `json:"thinking,omitempty"`
			ID       string          `json:"id,omitempty"`
			Name     string          `json:"name,omitempty"`
			Input    json.RawMessage `json:"input,omitempty"`
		} `json:"content"`
	} `json:"message"`
}

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
	AgentSlug         string `json:"agent_slug"`
	WorkingDirectory  string `json:"working_directory"`
	Model             string `json:"model"`
	SettingsProfileID string `json:"settings_profile_id"`
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

	session, err := s.chatSvc.CreateSession(r.Context(), req.AgentSlug, req.WorkingDirectory, req.Model, req.SettingsProfileID)
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
	// blocks accumulates ordered content blocks (thinking/text/tool_use) across
	// all assistant events so they can be persisted and re-rendered after reload.
	var blocks []storage.MessageBlock
	// tokens accumulates usage across all TypeResult events (multi-turn sessions
	// may emit more than one).
	var tokens tokenAccumulator
	// pendingInput holds the AskUserQuestion tool input from the most recent
	// TypeAssistant event; non-nil means the agent asked the user something and
	// we need to pause and collect the answer before the conversation can continue.
	var pendingInput json.RawMessage

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

			switch event.Type {
			case claude.TypeAssistant:
				// Parse content blocks to persist them for post-reload rendering.
				var ae assistantEventRaw
				if err := json.Unmarshal(event.Raw, &ae); err == nil {
					for _, blk := range ae.Message.Content {
						switch blk.Type {
						case "thinking":
							if blk.Thinking != "" {
								blocks = append(blocks, storage.MessageBlock{Type: "thinking", Text: blk.Thinking})
							}
						case "text":
							if blk.Text != "" {
								blocks = append(blocks, storage.MessageBlock{Type: "text", Text: blk.Text})
							}
						case "tool_use":
							blocks = append(blocks, storage.MessageBlock{
								Type:  "tool_use",
								ID:    blk.ID,
								Name:  blk.Name,
								Input: blk.Input,
							})
						}
					}
				}
				// Detect AskUserQuestion tool_use so we know to pause on TypeResult.
				if input := extractAskUserQuestionInput(event.Raw); input != nil {
					pendingInput = input
					s.logger.Info("AskUserQuestion detected in stream", "session_id", id)
				}

			case claude.TypeResult:
				if event.Result == nil {
					continue
				}
				sdkSessionID = event.Result.SessionID
				tokens.add(event.Result)
				if event.Result.IsError {
					return
				}
				assistantText = event.Result.Result

				if pendingInput != nil {
					// Agent called AskUserQuestion — tell the frontend and wait for
					// the user's answer before continuing the session.
					s.logger.Info("sending user_input_required, waiting for answer", "session_id", id)
					sendSSEEvent(w, flusher, "user_input_required", map[string]any{
						"input": pendingInput,
					})
					pendingInput = nil

					select {
					case answer := <-inputCh:
						s.logger.Info("received user answer, resuming session", "session_id", id)
						if err := agentSession.Send(answer); err != nil {
							s.logger.Error("inject answer failed", "session_id", id, "error", err)
							return
						}
						assistantText = "" // reset for the next turn
					case <-r.Context().Done():
						return
					}
					// Continue event loop — second turn events are incoming.
				} else {
					// No pending question: this is the final result.
					goto done
				}
			}

		case qInput := <-questionCh:
			// PermissionHandler path: subprocess paused on can_use_tool for
			// AskUserQuestion (future-proofing; currently AskUserQuestion does
			// not go through can_use_tool in any permission mode).
			pendingInput = nil // prevent double-trigger from TypeResult
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

	if err := s.chatSvc.CommitMessage(r.Context(), chatSession, assistantText, sdkSessionID, isFirstMessage, blocks, tokens.toUsageStats()); err != nil {
		s.logger.Error("commit message failed", "session_id", id, "error", err)
	}
}

// extractAskUserQuestionInput parses a raw assistant event and returns the
// input JSON of the first AskUserQuestion tool_use content block, or nil.
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
