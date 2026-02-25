package service

import (
	"context"
	"fmt"
	"log/slog"
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
	CreateSession(ctx context.Context, agentSlug string) (*storage.ChatSession, error)

	// DeleteSession removes a session and all its messages.
	DeleteSession(ctx context.Context, id string) error

	// BeginMessage stores the user message, resolves the agent config, and starts
	// the agent stream. The caller must consume all events from the returned stream
	// and then call CommitMessage.
	BeginMessage(ctx context.Context, sessionID, content string) (*claude.Stream, *storage.ChatSession, error)

	// CommitMessage persists the assistant response and updates session metadata.
	CommitMessage(ctx context.Context, session *storage.ChatSession, assistantText, sdkSessionID string, isFirstMessage bool) error
}

// chatService is the default implementation of ChatService.
type chatService struct {
	chatRepo    storage.ChatStore
	agentRepo   storage.AgentStore
	mcpRegistry *config.MCPRegistry
	localMCP    *tools.LocalMCPConfig
	logger      *slog.Logger
}

// NewChatService constructs a ChatService backed by the provided repositories.
func NewChatService(
	chatRepo storage.ChatStore,
	agentRepo storage.AgentStore,
	mcpRegistry *config.MCPRegistry,
	localMCP *tools.LocalMCPConfig,
	logger *slog.Logger,
) ChatService {
	return &chatService{
		chatRepo:    chatRepo,
		agentRepo:   agentRepo,
		mcpRegistry: mcpRegistry,
		localMCP:    localMCP,
		logger:      logger,
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

func (s *chatService) CreateSession(_ context.Context, agentSlug string) (*storage.ChatSession, error) {
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

	session, err := s.chatRepo.CreateSession(agentSlug)
	if err != nil {
		return nil, fmt.Errorf("creating session: %w", err)
	}

	s.logger.Info("chat session created", "session_id", session.ID, "agent_slug", agentSlug)
	return session, nil
}

func (s *chatService) DeleteSession(_ context.Context, id string) error {
	if err := s.chatRepo.DeleteSession(id); err != nil {
		return fmt.Errorf("deleting session %q: %w", id, err)
	}
	s.logger.Info("chat session deleted", "session_id", id)
	return nil
}

func (s *chatService) BeginMessage(ctx context.Context, sessionID, content string) (*claude.Stream, *storage.ChatSession, error) {
	session, err := s.chatRepo.GetSession(sessionID)
	if err != nil {
		return nil, nil, fmt.Errorf("loading session: %w", err)
	}
	if session == nil {
		return nil, nil, &NotFoundError{Resource: "chat", ID: sessionID}
	}

	// Resolve agent config (nil is valid — means no-agent chat).
	var agentCfg *config.AgentConfig
	if session.AgentSlug != "" {
		agentCfg, err = s.agentRepo.Get(session.AgentSlug)
		if err != nil {
			return nil, nil, fmt.Errorf("loading agent: %w", err)
		}
		if agentCfg == nil {
			return nil, nil, &NotFoundError{Resource: "agent", ID: session.AgentSlug}
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

	stream, err := agent.StreamAgent(ctx, agentCfg, content, agent.RunOptions{
		SessionID:     session.SDKSession,
		LocalToolsMCP: s.localMCP,
		MCPRegistry:   s.mcpRegistry,
	})
	if err != nil {
		return nil, nil, fmt.Errorf("starting agent stream: %w", err)
	}

	s.logger.Info("message stream started", "session_id", sessionID)
	return stream, session, nil
}

func (s *chatService) CommitMessage(_ context.Context, session *storage.ChatSession, assistantText, sdkSessionID string, _ bool) error {
	if assistantText != "" {
		msg := storage.ChatMessage{
			Role:      "assistant",
			Content:   assistantText,
			Timestamp: time.Now().UTC(),
		}
		if err := s.chatRepo.AppendMessage(session.ID, msg); err != nil {
			return fmt.Errorf("storing assistant message: %w", err)
		}
	}

	session.SDKSession = sdkSessionID
	session.UpdatedAt = time.Now().UTC()

	if err := s.chatRepo.UpdateSession(session); err != nil {
		return fmt.Errorf("updating session: %w", err)
	}

	s.logger.Info("message committed", "session_id", session.ID, "sdk_session_id", sdkSessionID)
	return nil
}
