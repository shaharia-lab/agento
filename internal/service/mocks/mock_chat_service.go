package mocks

import (
	"context"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/agent"
	"github.com/shaharia-lab/agento/internal/storage"
)

// MockChatService is a mock implementation of service.ChatService.
type MockChatService struct {
	mock.Mock
}

//nolint:revive
func (m *MockChatService) ListSessions(ctx context.Context) ([]*storage.ChatSession, error) {
	args := m.Called(ctx)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*storage.ChatSession), args.Error(1)
}

//nolint:revive
func (m *MockChatService) GetSession(ctx context.Context, id string) (*storage.ChatSession, error) {
	args := m.Called(ctx, id)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.ChatSession), args.Error(1)
}

//nolint:revive
func (m *MockChatService) GetSessionWithMessages(ctx context.Context, id string) (*storage.ChatSession, []storage.ChatMessage, error) {
	args := m.Called(ctx, id)
	var session *storage.ChatSession
	if args.Get(0) != nil {
		session = args.Get(0).(*storage.ChatSession)
	}
	var msgs []storage.ChatMessage
	if args.Get(1) != nil {
		msgs = args.Get(1).([]storage.ChatMessage)
	}
	return session, msgs, args.Error(2)
}

//nolint:revive
func (m *MockChatService) CreateSession(ctx context.Context, agentSlug, workingDir, model, settingsProfileID string) (*storage.ChatSession, error) {
	args := m.Called(ctx, agentSlug, workingDir, model, settingsProfileID)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.ChatSession), args.Error(1)
}

//nolint:revive
func (m *MockChatService) DeleteSession(ctx context.Context, id string) error {
	args := m.Called(ctx, id)
	return args.Error(0)
}

//nolint:revive
func (m *MockChatService) BeginMessage(ctx context.Context, sessionID, content string, opts agent.RunOptions) (*claude.Session, *storage.ChatSession, error) {
	args := m.Called(ctx, sessionID, content, opts)
	var session *claude.Session
	if args.Get(0) != nil {
		session = args.Get(0).(*claude.Session)
	}
	var chatSession *storage.ChatSession
	if args.Get(1) != nil {
		chatSession = args.Get(1).(*storage.ChatSession)
	}
	return session, chatSession, args.Error(2)
}

//nolint:revive
func (m *MockChatService) CommitMessage(ctx context.Context, session *storage.ChatSession, assistantText, sdkSessionID string, isFirstMessage bool, blocks []storage.MessageBlock, usage agent.UsageStats) error {
	args := m.Called(ctx, session, assistantText, sdkSessionID, isFirstMessage, blocks, usage)
	return args.Error(0)
}

//nolint:revive
func (m *MockChatService) UpdateSession(ctx context.Context, session *storage.ChatSession) error {
	args := m.Called(ctx, session)
	return args.Error(0)
}
