package service

import (
	"context"
	"fmt"
	"log/slog"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/metric"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/agent"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/integrations"
	"github.com/shaharia-lab/agento/internal/storage"
	"github.com/shaharia-lab/agento/internal/telemetry"
	"github.com/shaharia-lab/agento/internal/tools"
)

// chatSessionIDAttr is the OTel attribute key for chat session IDs.
const chatSessionIDAttr = "chat.session_id"

// ChatService defines the business logic interface for managing chat sessions
// and streaming agent responses.
type ChatService interface {
	// ListSessions returns all chat sessions ordered by most recently updated.
	ListSessions(ctx context.Context) ([]*storage.ChatSession, error)

	// GetSession returns session metadata, or nil if not found.
	GetSession(ctx context.Context, id string) (*storage.ChatSession, error)

	// GetSessionWithMessages returns the session and its full message history.
	GetSessionWithMessages(ctx context.Context, id string) (*storage.ChatSession, []storage.ChatMessage, error)

	// CreateSession starts a new chat session. agentSlug may be empty for no-agent chat.
	// workingDir and model are stored with the session and used during message processing.
	// settingsProfileID optionally links the session to a named Claude settings profile.
	CreateSession(
		ctx context.Context, agentSlug, workingDir, model, settingsProfileID string,
	) (*storage.ChatSession, error)

	// DeleteSession removes a session and all its messages.
	DeleteSession(ctx context.Context, id string) error

	// BulkDeleteSessions removes multiple sessions and their messages.
	BulkDeleteSessions(ctx context.Context, ids []string) error

	// BeginMessage stores the user message, resolves the agent config, and starts
	// a persistent agent session. The caller must consume events from session.Events()
	// (breaking at each TypeResult), inject follow-up messages via session.Send() as
	// needed, call session.Close() when done, and then call CommitMessage.
	BeginMessage(
		ctx context.Context, sessionID, content string, opts agent.RunOptions,
	) (*claude.Session, *storage.ChatSession, error)

	// CommitMessage persists the assistant response and updates session metadata.
	// blocks contains the ordered content blocks (thinking/text/tool_use) captured
	// during streaming so they can be re-rendered faithfully after a page reload.
	// usage holds the cumulative token counts for the completed turn(s); they are
	// added to the session's running totals.
	CommitMessage(
		ctx context.Context, session *storage.ChatSession,
		assistantText, sdkSessionID string, isFirstMessage bool,
		blocks []storage.MessageBlock, usage agent.UsageStats,
	) error

	// UpdateSession persists updated session metadata (e.g. after linking an SDK session ID).
	UpdateSession(ctx context.Context, session *storage.ChatSession) error
}

// chatService is the default implementation of ChatService.
type chatService struct {
	chatRepo            storage.ChatStore
	agentRepo           storage.AgentStore
	mcpRegistry         *config.MCPRegistry
	localMCP            *tools.LocalMCPConfig
	integrationRegistry *integrations.IntegrationRegistry
	settingsMgr         *config.SettingsManager
	logger              *slog.Logger
}

// NewChatService constructs a ChatService backed by the provided repositories.
func NewChatService(
	chatRepo storage.ChatStore,
	agentRepo storage.AgentStore,
	mcpRegistry *config.MCPRegistry,
	localMCP *tools.LocalMCPConfig,
	integrationRegistry *integrations.IntegrationRegistry,
	settingsMgr *config.SettingsManager,
	logger *slog.Logger,
) ChatService {
	return &chatService{
		chatRepo:            chatRepo,
		agentRepo:           agentRepo,
		mcpRegistry:         mcpRegistry,
		localMCP:            localMCP,
		integrationRegistry: integrationRegistry,
		settingsMgr:         settingsMgr,
		logger:              logger,
	}
}

func (s *chatService) ListSessions(ctx context.Context) ([]*storage.ChatSession, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "chat.list_sessions")
	defer span.End()

	sessions, err := s.chatRepo.ListSessions(ctx)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("listing sessions: %w", err)
	}
	return sessions, nil
}

func (s *chatService) GetSession(ctx context.Context, id string) (*storage.ChatSession, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "chat.get_session")
	defer span.End()

	session, err := s.chatRepo.GetSession(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("getting session %q: %w", id, err)
	}
	return session, nil
}

func (s *chatService) GetSessionWithMessages(
	ctx context.Context, id string,
) (*storage.ChatSession, []storage.ChatMessage, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "chat.get_session_with_messages")
	defer span.End()

	session, msgs, err := s.chatRepo.GetSessionWithMessages(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, nil, fmt.Errorf("getting session with messages %q: %w", id, err)
	}
	return session, msgs, nil
}

func (s *chatService) CreateSession(
	ctx context.Context, agentSlug, workingDir, model, settingsProfileID string,
) (*storage.ChatSession, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "chat.create_session")
	defer span.End()

	span.SetAttributes(
		attribute.String("chat.agent_slug", agentSlug),
		attribute.String("chat.model", model),
	)

	// Validate agent slug if provided.
	if agentSlug != "" {
		agentCfg, err := s.agentRepo.Get(ctx, agentSlug)
		if err != nil {
			span.RecordError(err)
			span.SetStatus(codes.Error, err.Error())
			return nil, fmt.Errorf("looking up agent: %w", err)
		}
		if agentCfg == nil {
			return nil, &NotFoundError{Resource: "agent", ID: agentSlug}
		}
	}

	session, err := s.chatRepo.CreateSession(ctx, agentSlug, workingDir, model, settingsProfileID)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("creating session: %w", err)
	}

	span.SetAttributes(attribute.String(chatSessionIDAttr, session.ID))

	if instr := telemetry.GetGlobalInstruments(); instr != nil {
		instr.ChatSessionsCreated.Add(ctx, 1, metric.WithAttributes(
			attribute.String("agent_slug", agentSlug),
		))
	}

	s.logger.Info("chat session created",
		"session_id", session.ID,
		"agent_slug", agentSlug,
		"settings_profile_id", settingsProfileID,
	)
	return session, nil
}

func (s *chatService) DeleteSession(ctx context.Context, id string) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "chat.delete_session")
	defer span.End()

	span.SetAttributes(attribute.String(chatSessionIDAttr, id))

	if err := s.chatRepo.DeleteSession(ctx, id); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf("deleting session %q: %w", id, err)
	}

	if instr := telemetry.GetGlobalInstruments(); instr != nil {
		instr.ChatSessionsDeleted.Add(ctx, 1)
	}

	s.logger.Info("chat session deleted", "session_id", id)
	return nil
}

func (s *chatService) BulkDeleteSessions(ctx context.Context, ids []string) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "chat.bulk_delete_sessions")
	defer span.End()

	if err := s.chatRepo.BulkDeleteSessions(ctx, ids); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf("bulk deleting sessions: %w", err)
	}
	s.logger.Info("chat sessions bulk deleted", "count", len(ids))
	return nil
}

func (s *chatService) BeginMessage(
	ctx context.Context, sessionID, content string,
	opts agent.RunOptions,
) (*claude.Session, *storage.ChatSession, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "chat.begin_message")
	defer span.End()

	span.SetAttributes(attribute.String(chatSessionIDAttr, sessionID))

	session, err := s.chatRepo.GetSession(ctx, sessionID)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, nil, fmt.Errorf("loading session: %w", err)
	}
	if session == nil {
		return nil, nil, &NotFoundError{Resource: "chat", ID: sessionID}
	}

	span.SetAttributes(
		attribute.String("chat.agent_slug", session.AgentSlug),
		attribute.String("chat.model", session.Model),
	)

	agentCfg, err := s.resolveAgentConfig(ctx, session)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, nil, err
	}

	userMsg := storage.ChatMessage{
		Role:      "user",
		Content:   content,
		Timestamp: time.Now().UTC(),
	}
	if appendErr := s.chatRepo.AppendMessage(ctx, sessionID, userMsg); appendErr != nil {
		span.RecordError(appendErr)
		span.SetStatus(codes.Error, appendErr.Error())
		return nil, nil, fmt.Errorf("storing user message: %w", appendErr)
	}

	s.populateRunOptions(&opts, session)

	agentSession, err := agent.StartSession(ctx, agentCfg, content, opts)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, nil, fmt.Errorf("starting agent session: %w", err)
	}

	s.logger.Info("agent session started", "session_id", sessionID)
	return agentSession, session, nil
}

// resolveAgentConfig loads the agent config for the session, or synthesizes a
// minimal one for no-agent (direct) chat sessions.
func (s *chatService) resolveAgentConfig(
	ctx context.Context, session *storage.ChatSession,
) (*config.AgentConfig, error) {
	if session.AgentSlug != "" {
		agentCfg, err := s.agentRepo.Get(ctx, session.AgentSlug)
		if err != nil {
			return nil, fmt.Errorf("loading agent: %w", err)
		}
		if agentCfg == nil {
			return nil, &NotFoundError{Resource: "agent", ID: session.AgentSlug}
		}
		return agentCfg, nil
	}

	model := session.Model
	if model == "" && s.settingsMgr != nil {
		model = s.settingsMgr.Get().DefaultModel
	}
	return &config.AgentConfig{
		Model:    model,
		Thinking: "adaptive",
	}, nil
}

// populateRunOptions fills in the run options from service dependencies and session state.
func (s *chatService) populateRunOptions(opts *agent.RunOptions, session *storage.ChatSession) {
	if session.SDKSession != "" {
		// Resume existing Claude CLI session.
		opts.ResumeSessionID = session.SDKSession
	} else {
		// New session: pin the Claude CLI session ID to the chat ID so both
		// identifiers stay in sync — no more two-ID confusion.
		opts.CustomSessionID = session.ID
	}
	opts.LocalToolsMCP = s.localMCP
	opts.MCPRegistry = s.mcpRegistry
	opts.IntegrationRegistry = s.integrationRegistry
	opts.WorkingDir = session.WorkingDir

	settingsFilePath, resolveErr := config.LoadProfileFilePath(session.SettingsProfileID)
	if resolveErr != nil {
		s.logger.Warn("failed to resolve settings profile path, using default", "error", resolveErr)
	} else {
		opts.SettingsFilePath = settingsFilePath
	}
}

func (s *chatService) CommitMessage(
	ctx context.Context, session *storage.ChatSession,
	assistantText, sdkSessionID string, _ bool,
	blocks []storage.MessageBlock, usage agent.UsageStats,
) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "chat.commit_message")
	defer span.End()

	if assistantText != "" {
		msg := storage.ChatMessage{
			Role:      "assistant",
			Content:   assistantText,
			Timestamp: time.Now().UTC(),
			Blocks:    blocks,
		}
		if err := s.chatRepo.AppendMessage(ctx, session.ID, msg); err != nil {
			span.RecordError(err)
			span.SetStatus(codes.Error, err.Error())
			return fmt.Errorf("storing assistant message: %w", err)
		}
	}

	session.SDKSession = sdkSessionID
	session.UpdatedAt = time.Now().UTC()
	// Accumulate token usage into the session running totals.
	session.TotalInputTokens += usage.InputTokens
	session.TotalOutputTokens += usage.OutputTokens
	session.TotalCacheCreationTokens += usage.CacheCreationInputTokens
	session.TotalCacheReadTokens += usage.CacheReadInputTokens

	if err := s.chatRepo.UpdateSession(ctx, session); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf("updating session: %w", err)
	}

	s.logger.Info("message committed", "session_id", session.ID, "sdk_session_id", sdkSessionID)
	return nil
}

func (s *chatService) UpdateSession(ctx context.Context, session *storage.ChatSession) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "chat.update_session")
	defer span.End()

	session.UpdatedAt = time.Now().UTC()
	if err := s.chatRepo.UpdateSession(ctx, session); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf("updating session: %w", err)
	}
	return nil
}
