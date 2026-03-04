package api

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"time"
	"unicode/utf8"

	"github.com/go-chi/chi/v5"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/trace"

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
	WebSearchRequests        int
}

func (t *tokenAccumulator) add(r *claude.Result) {
	if r == nil {
		return
	}
	t.InputTokens += r.Usage.InputTokens
	t.OutputTokens += r.Usage.OutputTokens
	t.CacheCreationInputTokens += r.Usage.CacheCreationInputTokens
	t.CacheReadInputTokens += r.Usage.CacheReadInputTokens
	t.WebSearchRequests += r.Usage.WebSearchRequests
}

func (t *tokenAccumulator) toUsageStats() agent.UsageStats {
	return agent.UsageStats{
		InputTokens:              t.InputTokens,
		OutputTokens:             t.OutputTokens,
		CacheCreationInputTokens: t.CacheCreationInputTokens,
		CacheReadInputTokens:     t.CacheReadInputTokens,
		WebSearchRequests:        t.WebSearchRequests,
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
		s.writeError(w, http.StatusInternalServerError, "failed to list chats")
		return
	}
	s.writeJSON(w, http.StatusOK, sessions)
}

func (s *Server) handleCreateChat(w http.ResponseWriter, r *http.Request) {
	var req createChatRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	session, err := s.chatSvc.CreateSession(
		r.Context(), req.AgentSlug, req.WorkingDirectory, req.Model, req.SettingsProfileID,
	)
	if err != nil {
		var nfe *service.NotFoundError
		if errors.As(err, &nfe) {
			s.writeError(w, http.StatusNotFound, nfe.Error())
			return
		}
		s.logger.Error("create chat failed", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to create chat")
		return
	}
	s.writeJSON(w, http.StatusCreated, session)
}

func (s *Server) handleGetChat(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	session, messages, err := s.chatSvc.GetSessionWithMessages(r.Context(), id)
	if err != nil {
		s.logger.Error("get chat failed", "session_id", id, "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to get chat")
		return
	}
	if session == nil {
		s.writeError(w, http.StatusNotFound, "chat not found")
		return
	}
	s.writeJSON(w, http.StatusOK, map[string]any{
		"session":  session,
		"messages": messages,
	})
}

func (s *Server) handleDeleteChat(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := s.chatSvc.DeleteSession(r.Context(), id); err != nil {
		var nfe *service.NotFoundError
		if errors.As(err, &nfe) {
			s.writeError(w, http.StatusNotFound, nfe.Error())
			return
		}
		s.logger.Error("delete chat failed", "session_id", id, "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to delete chat")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleBulkDeleteChats(w http.ResponseWriter, r *http.Request) {
	var req BulkDeleteRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}
	if len(req.IDs) == 0 {
		s.writeError(w, http.StatusBadRequest, "ids must not be empty")
		return
	}
	if len(req.IDs) > maxQueryLimit {
		s.writeError(w, http.StatusBadRequest, "too many ids (max 500)")
		return
	}
	if err := s.chatSvc.BulkDeleteSessions(r.Context(), req.IDs); err != nil {
		s.logger.Error("bulk delete chats failed", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to delete chats")
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
	toolSpans     map[string]agent.ToolSpanEntry // in-flight tool_use spans keyed by tool_use_id
}

// eventProcessor captures the shared context for processing agent events during
// a single SSE stream. It replaces the long parameter lists of consumeAgentEvents,
// processAgentEvent, and handlePendingUserInput.
type eventProcessor struct {
	server       *Server
	r            *http.Request
	id           string
	agentSession *claude.Session
	flusher      http.Flusher
	w            http.ResponseWriter
	chs          sendMessageChannels
	execSpan     trace.Span // parent span covering the full streaming window
}

func (s *Server) handleSendMessage(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	var req sendMessageRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}
	if req.Content == "" {
		s.writeError(w, http.StatusBadRequest, "content is required")
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

	// chat.begin_message ends immediately when StartSession returns (~ms).
	// This span covers the actual agent streaming + commit (~seconds).
	_, execSpan := otel.Tracer("agento").Start(r.Context(), "chat.agent_execution")
	execSpan.SetAttributes(
		attribute.String("chat.session_id", id),
		attribute.String("chat.agent_slug", chatSession.AgentSlug),
	)

	isFirstMessage := chatSession.Title == "New Chat"
	state := s.streamAgentSession(w, r, id, agentSession, chs, execSpan)

	if isFirstMessage {
		chatSession.Title = truncateTitle(req.Content, 60)
	}
	s.commitMessage(execSpan, chatSession, state, isFirstMessage, id)
}

// streamAgentSession sets up the SSE response, registers the live session,
// runs the event loop, and returns the accumulated stream state.
// execSpan is ended by the caller (commitMessage).
func (s *Server) streamAgentSession(
	w http.ResponseWriter, r *http.Request, id string,
	agentSession *claude.Session, chs sendMessageChannels,
	execSpan trace.Span,
) streamState {
	flusher, ok := s.prepareSSEResponse(w, agentSession)
	if !ok {
		execSpan.End()
		return streamState{}
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

	ep := &eventProcessor{
		server:       s,
		r:            r,
		id:           id,
		agentSession: agentSession,
		flusher:      flusher,
		w:            w,
		chs:          chs,
		execSpan:     execSpan,
	}
	state := ep.consumeAgentEvents()
	agent.FlushToolSpans(state.toolSpans) // close any spans not ended by a tool_result
	return state
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
		s.sendSSEEvent(w, nil, "error", map[string]string{
			"error": "streaming not supported",
		})
		if cerr := agentSession.Close(); cerr != nil {
			s.logger.Error("close agent session", "error", cerr)
		}
	}
	return flusher, ok
}

func (s *Server) commitMessage(
	execSpan trace.Span,
	chatSession *storage.ChatSession,
	state streamState, isFirstMessage bool, id string,
) {
	defer execSpan.End()

	// Detach from the SSE request context so the commit always succeeds even
	// if the client disconnected mid-stream, but preserve the trace lineage so
	// chat.commit_message appears as a child of chat.agent_execution.
	commitCtx, cancel := context.WithTimeout(
		trace.ContextWithSpanContext(context.Background(), execSpan.SpanContext()),
		10*time.Second,
	)
	defer cancel()

	if err := s.chatSvc.CommitMessage(
		commitCtx, chatSession,
		state.assistantText, state.sdkSessionID,
		isFirstMessage, state.blocks,
		state.tokens.toUsageStats(),
	); err != nil {
		execSpan.RecordError(err)
		execSpan.SetStatus(codes.Error, err.Error())
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
		s.writeError(w, http.StatusNotFound, nfe.Error())
		return
	}
	s.logger.Error("begin message failed", "session_id", id, "error", err)
	s.writeError(w, http.StatusInternalServerError, "failed to start message")
}

func (ep *eventProcessor) consumeAgentEvents() streamState {
	state := streamState{toolSpans: make(map[string]agent.ToolSpanEntry)}
	eventsCh := ep.agentSession.Events()

	for {
		select {
		case event, ok := <-eventsCh:
			if !ok {
				return state
			}
			if len(event.Raw) > 0 {
				sendSSERaw(ep.w, ep.flusher, string(event.Type), event.Raw)
			}
			if ep.processAgentEvent(event, &state) {
				return state
			}

		case qInput := <-ep.chs.questionCh:
			state.pendingInput = nil
			ep.server.sendSSEEvent(ep.w, ep.flusher, "user_input_required", map[string]any{"input": qInput})

		case pr := <-ep.chs.permissionReqCh:
			ep.server.sendSSEEvent(ep.w, ep.flusher, "permission_request", pr)

		case <-ep.r.Context().Done():
			return state
		}
	}
}

func (ep *eventProcessor) processAgentEvent(event claude.Event, state *streamState) bool {
	switch event.Type {
	case claude.TypeAssistant:
		state.blocks = appendAssistantBlocks(state.blocks, event.Raw)
		agent.OpenToolSpans(ep.r.Context(), ep.execSpan, event.Raw, state.toolSpans)
		if input := extractAskUserQuestionInput(event.Raw); input != nil {
			state.pendingInput = input
			ep.server.logger.Info("AskUserQuestion detected in stream", "session_id", ep.id)
		}

	case claude.TypeSystem:
		agent.AddSystemInitEvent(ep.execSpan, event.System)

	case claude.TypeToolProgress:
		agent.RecordToolProgress(event.ToolProgress, state.toolSpans)

	case agent.MessageTypeUser:
		agent.CloseToolSpans(event.Raw, state.toolSpans)

	case claude.TypeResult:
		if event.Result == nil {
			return false
		}
		agent.EnrichSpanFromResult(ep.execSpan, event.Result, event.Raw)
		state.tokens.add(event.Result)
		if event.Result.IsError {
			// Clear the stale SDK session ID so the next attempt starts a fresh
			// session instead of endlessly retrying a session that no longer exists
			// (e.g. "No conversation found with session ID: ...").
			state.sdkSessionID = ""
			return true
		}
		state.sdkSessionID = event.Result.SessionID
		state.assistantText = event.Result.Result

		if state.pendingInput == nil {
			return true // final result
		}
		return ep.handlePendingUserInput(state)
	}
	return false
}

func (ep *eventProcessor) handlePendingUserInput(state *streamState) bool {
	ep.server.logger.Info("sending user_input_required, waiting for answer", "session_id", ep.id)
	ep.server.sendSSEEvent(ep.w, ep.flusher, "user_input_required", map[string]any{"input": state.pendingInput})
	state.pendingInput = nil

	select {
	case answer := <-ep.chs.inputCh:
		ep.server.logger.Info("received user answer, resuming session", "session_id", ep.id)
		if err := ep.agentSession.Send(answer); err != nil {
			ep.server.logger.Error("inject answer failed", "session_id", ep.id, "error", err)
			return true
		}
		state.assistantText = ""
		return false // continue event loop
	case <-ep.r.Context().Done():
		return true
	}
}

func appendAssistantBlocks(blocks []storage.MessageBlock, raw json.RawMessage) []storage.MessageBlock {
	var ae assistantEventRaw
	if json.Unmarshal(raw, &ae) != nil {
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
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	ls, ok := s.liveSessions.get(id)
	if !ok {
		s.writeError(w, http.StatusConflict, "no active session for this chat")
		return
	}

	select {
	case ls.permissionRespCh <- req.Allow:
		w.WriteHeader(http.StatusNoContent)
	default:
		s.writeError(w, http.StatusConflict, "session is not currently awaiting a permission response")
	}
}

// handleStopSession gracefully stops the active agent session for a chat.
// It sends SIGINT to the subprocess, giving it a chance to finish cleanly.
// The deferred cleanup in handleSendMessage is responsible for the final
// session.Close() call, so we do not call Close() here to avoid a double-close.
func (s *Server) handleStopSession(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	ls, ok := s.liveSessions.get(id)
	if !ok {
		s.writeError(w, http.StatusConflict, "no active session for this chat")
		return
	}

	// Interrupt sends SIGINT to the subprocess, giving it a chance to
	// finish the current operation and write the session to disk.
	// The SSE handler's deferred cleanup will call Close() once the
	// event loop exits, so we do not call Close() here.
	if err := ls.session.Interrupt(); err != nil {
		s.logger.Warn("interrupt session failed", "session_id", id, "error", err)
	}

	w.WriteHeader(http.StatusNoContent)
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
	if json.Unmarshal(raw, &msg) != nil {
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
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}
	if req.Answer == "" {
		s.writeError(w, http.StatusBadRequest, "answer is required")
		return
	}

	ls, ok := s.liveSessions.get(id)
	if !ok {
		s.writeError(w, http.StatusConflict, "no active session awaiting input for this chat")
		return
	}

	select {
	case ls.inputCh <- req.Answer:
		w.WriteHeader(http.StatusNoContent)
	default:
		s.writeError(w, http.StatusConflict, "session is not currently awaiting input")
	}
}
