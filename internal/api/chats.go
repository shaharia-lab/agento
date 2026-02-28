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

// permReq carries a tool permission request from the permission handler goroutine
// to the SSE HTTP handler goroutine so it can be forwarded to the frontend.
type permReq struct {
	ToolName string          `json:"tool_name"`
	Input    json.RawMessage `json:"input,omitempty"`
}

// sendSSERaw writes a raw JSON payload as an SSE event without re-marshaling.
func sendSSERaw(w http.ResponseWriter, flusher http.Flusher, event string, raw json.RawMessage) {
	if _, err := w.Write([]byte("event: " + event + "\ndata: ")); err != nil {
		return
	}
	if _, err := w.Write(raw); err != nil {
		return
	}
	if _, err := w.Write([]byte("\n\n")); err != nil {
		return
	}
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
		writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	session, err := s.chatSvc.CreateSession(
		r.Context(), req.AgentSlug, req.WorkingDirectory, req.Model, req.SettingsProfileID,
	)
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

// sendMessageChannels groups the channels used to coordinate between the
// permission handler goroutine and the SSE HTTP handler goroutine.
type sendMessageChannels struct {
	inputCh          chan string
	questionCh       chan json.RawMessage
	permissionReqCh  chan permReq
	permissionRespCh chan bool
}

// streamState tracks the mutable state accumulated while consuming agent events.
type streamState struct {
	assistantText string
	sdkSessionID  string
	blocks        []storage.MessageBlock
	tokens        tokenAccumulator
	pendingInput  json.RawMessage
}

func (s *Server) handleSendMessage(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	var req sendMessageRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}
	if req.Content == "" {
		writeError(w, http.StatusBadRequest, "content is required")
		return
	}

	chs := newSendMessageChannels()
	permHandler := s.buildPermissionHandler(r, chs)

	agentSession, chatSession, err := s.chatSvc.BeginMessage(
		r.Context(), id, req.Content,
		agent.RunOptions{PermissionHandler: permHandler},
	)
	if err != nil {
		s.handleBeginMessageError(w, id, err)
		return
	}

	isFirstMessage := chatSession.Title == "New Chat"

	flusher, ok := s.prepareSSEResponse(w, agentSession)
	if !ok {
		return
	}

	s.liveSessions.put(id, &liveSession{
		session:          agentSession,
		inputCh:          chs.inputCh,
		permissionRespCh: chs.permissionRespCh,
	})
	defer func() {
		s.liveSessions.delete(id)
		if cerr := agentSession.Close(); cerr != nil {
			s.logger.Error("close agent session", "id", id, "error", cerr)
		}
		close(chs.questionCh)
	}()

	state := s.consumeAgentEvents(r, id, agentSession, flusher, w, chs)

	if isFirstMessage {
		chatSession.Title = truncateTitle(req.Content, 60)
	}
	s.commitMessage(r, chatSession, state, isFirstMessage, id)
}

func newSendMessageChannels() sendMessageChannels {
	return sendMessageChannels{
		inputCh:          make(chan string, 1),
		questionCh:       make(chan json.RawMessage, 4),
		permissionReqCh:  make(chan permReq, 4),
		permissionRespCh: make(chan bool, 1),
	}
}

func (s *Server) prepareSSEResponse(
	w http.ResponseWriter, agentSession *claude.Session,
) (http.Flusher, bool) {
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)

	flusher, ok := w.(http.Flusher)
	if !ok {
		sendSSEEvent(w, nil, "error", map[string]string{
			"error": "streaming not supported",
		})
		if cerr := agentSession.Close(); cerr != nil {
			s.logger.Error("close agent session", "error", cerr)
		}
	}
	return flusher, ok
}

func (s *Server) commitMessage(
	r *http.Request, chatSession *storage.ChatSession,
	state streamState, isFirstMessage bool, id string,
) {
	if err := s.chatSvc.CommitMessage(
		r.Context(), chatSession,
		state.assistantText, state.sdkSessionID,
		isFirstMessage, state.blocks,
		state.tokens.toUsageStats(),
	); err != nil {
		s.logger.Error("commit message failed", "session_id", id, "error", err)
	}
}

func (s *Server) buildPermissionHandler(r *http.Request, chs sendMessageChannels) claude.PermissionHandler {
	return func(toolName string, input json.RawMessage, _ claude.PermissionContext) claude.PermissionResult {
		if toolName == "AskUserQuestion" {
			return s.handleAskUserQuestionPermission(r, input, chs)
		}
		return s.handleToolPermission(r, toolName, input, chs)
	}
}

func (s *Server) handleAskUserQuestionPermission(
	r *http.Request, input json.RawMessage,
	chs sendMessageChannels,
) claude.PermissionResult {
	select {
	case chs.questionCh <- input:
	default:
	}
	select {
	case answer := <-chs.inputCh:
		return claude.PermissionResult{Behavior: "deny", Message: answer}
	case <-r.Context().Done():
		return claude.PermissionResult{Behavior: "deny", Message: "request canceled"}
	}
}

func (s *Server) handleToolPermission(
	r *http.Request, toolName string,
	input json.RawMessage, chs sendMessageChannels,
) claude.PermissionResult {
	select {
	case chs.permissionReqCh <- permReq{ToolName: toolName, Input: input}:
	default:
	}
	select {
	case allow := <-chs.permissionRespCh:
		if allow {
			return claude.PermissionResult{Behavior: "allow"}
		}
		return claude.PermissionResult{Behavior: "deny", Message: "Permission denied by user"}
	case <-r.Context().Done():
		return claude.PermissionResult{Behavior: "deny", Message: "request canceled"}
	}
}

func (s *Server) handleBeginMessageError(w http.ResponseWriter, id string, err error) {
	var nfe *service.NotFoundError
	if errors.As(err, &nfe) {
		writeError(w, http.StatusNotFound, nfe.Error())
		return
	}
	s.logger.Error("begin message failed", "session_id", id, "error", err)
	writeError(w, http.StatusInternalServerError, "failed to start message")
}

func (s *Server) consumeAgentEvents(
	r *http.Request, id string, agentSession *claude.Session,
	flusher http.Flusher, w http.ResponseWriter, chs sendMessageChannels,
) streamState {
	var state streamState
	eventsCh := agentSession.Events()

	for {
		select {
		case event, ok := <-eventsCh:
			if !ok {
				return state
			}
			if len(event.Raw) > 0 {
				sendSSERaw(w, flusher, string(event.Type), event.Raw)
			}
			done := s.processAgentEvent(r, id, event, agentSession, flusher, w, chs, &state)
			if done {
				return state
			}

		case qInput := <-chs.questionCh:
			state.pendingInput = nil
			sendSSEEvent(w, flusher, "user_input_required", map[string]any{"input": qInput})

		case pr := <-chs.permissionReqCh:
			sendSSEEvent(w, flusher, "permission_request", pr)

		case <-r.Context().Done():
			return state
		}
	}
}

func (s *Server) processAgentEvent(
	r *http.Request, id string, event claude.Event,
	agentSession *claude.Session, flusher http.Flusher, w http.ResponseWriter,
	chs sendMessageChannels, state *streamState,
) bool {
	switch event.Type {
	case claude.TypeAssistant:
		state.blocks = appendAssistantBlocks(state.blocks, event.Raw)
		if input := extractAskUserQuestionInput(event.Raw); input != nil {
			state.pendingInput = input
			s.logger.Info("AskUserQuestion detected in stream", "session_id", id)
		}

	case claude.TypeResult:
		if event.Result == nil {
			return false
		}
		state.sdkSessionID = event.Result.SessionID
		state.tokens.add(event.Result)
		if event.Result.IsError {
			return true
		}
		state.assistantText = event.Result.Result

		if state.pendingInput == nil {
			return true // final result
		}
		return s.handlePendingUserInput(r, id, agentSession, flusher, w, chs, state)
	}
	return false
}

func (s *Server) handlePendingUserInput(
	r *http.Request, id string, agentSession *claude.Session,
	flusher http.Flusher, w http.ResponseWriter, chs sendMessageChannels,
	state *streamState,
) bool {
	s.logger.Info("sending user_input_required, waiting for answer", "session_id", id)
	sendSSEEvent(w, flusher, "user_input_required", map[string]any{"input": state.pendingInput})
	state.pendingInput = nil

	select {
	case answer := <-chs.inputCh:
		s.logger.Info("received user answer, resuming session", "session_id", id)
		if err := agentSession.Send(answer); err != nil {
			s.logger.Error("inject answer failed", "session_id", id, "error", err)
			return true
		}
		state.assistantText = ""
		return false // continue event loop
	case <-r.Context().Done():
		return true
	}
}

func appendAssistantBlocks(blocks []storage.MessageBlock, raw json.RawMessage) []storage.MessageBlock {
	var ae assistantEventRaw
	if err := json.Unmarshal(raw, &ae); err != nil {
		return blocks
	}
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
				Type: "tool_use", ID: blk.ID, Name: blk.Name, Input: blk.Input,
			})
		}
	}
	return blocks
}

func truncateTitle(s string, maxRunes int) string {
	if utf8.RuneCountInString(s) <= maxRunes {
		return s
	}
	runes := []rune(s)
	return string(runes[:maxRunes]) + "..."
}

// handlePermissionResponse receives the user's allow/deny decision for a pending
// tool permission request and unblocks the PermissionHandler goroutine.
func (s *Server) handlePermissionResponse(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	var req struct {
		Allow bool `json:"allow"`
	}
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	ls, ok := s.liveSessions.get(id)
	if !ok {
		writeError(w, http.StatusConflict, "no active session for this chat")
		return
	}

	select {
	case ls.permissionRespCh <- req.Allow:
		w.WriteHeader(http.StatusNoContent)
	default:
		writeError(w, http.StatusConflict, "session is not currently awaiting a permission response")
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
		writeError(w, http.StatusBadRequest, errInvalidJSONBody)
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
