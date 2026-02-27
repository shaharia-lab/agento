package service

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"time"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/agent"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/storage"
	"github.com/shaharia-lab/agento/internal/tools"
)

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
	CreateSession(ctx context.Context, agentSlug, workingDir, model, settingsProfileID string) (*storage.ChatSession, error)

	// DeleteSession removes a session and all its messages.
	DeleteSession(ctx context.Context, id string) error

	// BeginMessage stores the user message, resolves the agent config, and starts
	// a persistent agent session. The caller must consume events from session.Events()
	// (breaking at each TypeResult), inject follow-up messages via session.Send() as
	// needed, call session.Close() when done, and then call CommitMessage.
	BeginMessage(ctx context.Context, sessionID, content string, opts agent.RunOptions) (*claude.Session, *storage.ChatSession, error)

	// CommitMessage persists the assistant response and updates session metadata.
	// blocks contains the ordered content blocks (thinking/text/tool_use) captured
	// during streaming so they can be re-rendered faithfully after a page reload.
	// usage holds the cumulative token counts for the completed turn(s); they are
	// added to the session's running totals.
	CommitMessage(ctx context.Context, session *storage.ChatSession, assistantText, sdkSessionID string, isFirstMessage bool, blocks []storage.MessageBlock, usage agent.UsageStats) error

	// UpdateSession persists updated session metadata (e.g. after linking an SDK session ID).
	UpdateSession(ctx context.Context, session *storage.ChatSession) error
}

// chatService is the default implementation of ChatService.
type chatService struct {
	chatRepo     storage.ChatStore
	agentRepo    storage.AgentStore
	mcpRegistry  *config.MCPRegistry
	localMCP     *tools.LocalMCPConfig
	defaultModel string
	logger       *slog.Logger
}

// NewChatService constructs a ChatService backed by the provided repositories.
// defaultModel is used for no-agent (direct) chat sessions.
func NewChatService(
	chatRepo storage.ChatStore,
	agentRepo storage.AgentStore,
	mcpRegistry *config.MCPRegistry,
	localMCP *tools.LocalMCPConfig,
	defaultModel string,
	logger *slog.Logger,
) ChatService {
	return &chatService{
		chatRepo:     chatRepo,
		agentRepo:    agentRepo,
		mcpRegistry:  mcpRegistry,
		localMCP:     localMCP,
		defaultModel: defaultModel,
		logger:       logger,
	}
}

func (s *chatService) ListSessions(_ context.Context) ([]*storage.ChatSession, error) {
	sessions, err := s.chatRepo.ListSessions()
	if err != nil {
		return nil, fmt.Errorf("listing sessions: %w", err)
	}
	return sessions, nil
}

func (s *chatService) GetSession(_ context.Context, id string) (*storage.ChatSession, error) {
	session, err := s.chatRepo.GetSession(id)
	if err != nil {
		return nil, fmt.Errorf("getting session %q: %w", id, err)
	}
	return session, nil
}

func (s *chatService) GetSessionWithMessages(_ context.Context, id string) (*storage.ChatSession, []storage.ChatMessage, error) {
	session, msgs, err := s.chatRepo.GetSessionWithMessages(id)
	if err != nil {
		return nil, nil, fmt.Errorf("getting session with messages %q: %w", id, err)
	}
	return session, msgs, nil
}

func (s *chatService) CreateSession(_ context.Context, agentSlug, workingDir, model, settingsProfileID string) (*storage.ChatSession, error) {
	// Validate agent slug if provided.
	if agentSlug != "" {
		agentCfg, err := s.agentRepo.Get(agentSlug)
		if err != nil {
			return nil, fmt.Errorf("looking up agent: %w", err)
		}
		if agentCfg == nil {
			return nil, &NotFoundError{Resource: "agent", ID: agentSlug}
		}
	}

	session, err := s.chatRepo.CreateSession(agentSlug, workingDir, model, settingsProfileID)
	if err != nil {
		return nil, fmt.Errorf("creating session: %w", err)
	}

	s.logger.Info("chat session created", "session_id", session.ID, "agent_slug", agentSlug, "settings_profile_id", settingsProfileID)
	return session, nil
}

func (s *chatService) DeleteSession(_ context.Context, id string) error {
	if err := s.chatRepo.DeleteSession(id); err != nil {
		return fmt.Errorf("deleting session %q: %w", id, err)
	}
	s.logger.Info("chat session deleted", "session_id", id)
	return nil
}

func (s *chatService) BeginMessage(ctx context.Context, sessionID, content string, opts agent.RunOptions) (*claude.Session, *storage.ChatSession, error) {
	session, err := s.chatRepo.GetSession(sessionID)
	if err != nil {
		return nil, nil, fmt.Errorf("loading session: %w", err)
	}
	if session == nil {
		return nil, nil, &NotFoundError{Resource: "chat", ID: sessionID}
	}

	// Resolve agent config. For no-agent sessions, synthesize a minimal config
	// using the session's model (or the default model) so the SDK still has a model to target.
	var agentCfg *config.AgentConfig
	if session.AgentSlug != "" {
		agentCfg, err = s.agentRepo.Get(session.AgentSlug)
		if err != nil {
			return nil, nil, fmt.Errorf("loading agent: %w", err)
		}
		if agentCfg == nil {
			return nil, nil, &NotFoundError{Resource: "agent", ID: session.AgentSlug}
		}
	} else {
		// No-agent (direct) chat: use the session's model, falling back to the default.
		model := session.Model
		if model == "" {
			model = s.defaultModel
		}
		agentCfg = &config.AgentConfig{
			Model:    model,
			Thinking: "adaptive",
		}
	}

	// Change working directory if the session has one configured.
	if session.WorkingDir != "" {
		if err := os.Chdir(session.WorkingDir); err != nil {
			return nil, nil, fmt.Errorf("changing working directory: %w", err)
		}
	}

	// Persist the user message before starting the stream.
	userMsg := storage.ChatMessage{
		Role:      "user",
		Content:   content,
		Timestamp: time.Now().UTC(),
	}
	if err := s.chatRepo.AppendMessage(sessionID, userMsg); err != nil {
		return nil, nil, fmt.Errorf("storing user message: %w", err)
	}

	opts.SessionID = session.SDKSession
	opts.LocalToolsMCP = s.localMCP
	opts.MCPRegistry = s.mcpRegistry

	// Resolve the settings file path for the session's profile.
	settingsFilePath, resolveErr := config.LoadProfileFilePath(session.SettingsProfileID)
	if resolveErr != nil {
		s.logger.Warn("failed to resolve settings profile path, using default", "error", resolveErr)
	} else {
		opts.SettingsFilePath = settingsFilePath
	}

	agentSession, err := agent.StartSession(ctx, agentCfg, content, opts)
	if err != nil {
		return nil, nil, fmt.Errorf("starting agent session: %w", err)
	}

	s.logger.Info("agent session started", "session_id", sessionID)
	return agentSession, session, nil
}

func (s *chatService) CommitMessage(_ context.Context, session *storage.ChatSession, assistantText, sdkSessionID string, _ bool, blocks []storage.MessageBlock, usage agent.UsageStats) error {
	if assistantText != "" {
		msg := storage.ChatMessage{
			Role:      "assistant",
			Content:   assistantText,
			Timestamp: time.Now().UTC(),
			Blocks:    blocks,
		}
		if err := s.chatRepo.AppendMessage(session.ID, msg); err != nil {
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

	if err := s.chatRepo.UpdateSession(session); err != nil {
		return fmt.Errorf("updating session: %w", err)
	}

	s.logger.Info("message committed", "session_id", session.ID, "sdk_session_id", sdkSessionID)
	return nil
}

func (s *chatService) UpdateSession(_ context.Context, session *storage.ChatSession) error {
	session.UpdatedAt = time.Now().UTC()
	if err := s.chatRepo.UpdateSession(session); err != nil {
		return fmt.Errorf("updating session: %w", err)
	}
	return nil
}
