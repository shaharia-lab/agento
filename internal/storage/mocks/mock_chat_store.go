package mocks

import (
	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/storage"
)

// MockChatStore is a mock implementation of storage.ChatStore.
type MockChatStore struct {
	mock.Mock
}

//nolint:revive
func (m *MockChatStore) ListSessions() ([]*storage.ChatSession, error) {
	args := m.Called()
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*storage.ChatSession), args.Error(1)
}

//nolint:revive
func (m *MockChatStore) GetSession(id string) (*storage.ChatSession, error) {
	args := m.Called(id)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.ChatSession), args.Error(1)
}

//nolint:revive
func (m *MockChatStore) GetSessionWithMessages(id string) (*storage.ChatSession, []storage.ChatMessage, error) {
	args := m.Called(id)
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
func (m *MockChatStore) CreateSession(agentSlug, workingDir, model, settingsProfileID string) (*storage.ChatSession, error) {
	args := m.Called(agentSlug, workingDir, model, settingsProfileID)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*storage.ChatSession), args.Error(1)
}

//nolint:revive
func (m *MockChatStore) AppendMessage(sessionID string, msg storage.ChatMessage) error {
	args := m.Called(sessionID, msg)
	return args.Error(0)
}

//nolint:revive
func (m *MockChatStore) UpdateSession(session *storage.ChatSession) error {
	args := m.Called(session)
	return args.Error(0)
}

//nolint:revive
func (m *MockChatStore) DeleteSession(id string) error {
	args := m.Called(id)
	return args.Error(0)
}
