package service

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/agent"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/storage"
	"github.com/shaharia-lab/agento/internal/storage/mocks"
)

// newTestService creates a ChatService wired with the provided mocks. Fields
// that are unused by the methods under test (mcpRegistry, localMCP,
// integrationRegistry) are left nil.
func newTestService(chatRepo *mocks.MockChatStore, agentRepo *mocks.MockAgentStore) ChatService {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	return NewChatService(chatRepo, agentRepo, nil, nil, nil, nil, logger)
}

// ---------------------------------------------------------------------------
// ListSessions
// ---------------------------------------------------------------------------

func TestListSessions(t *testing.T) {
	now := time.Now().UTC()

	tests := []struct {
		name        string
		repoResult  []*storage.ChatSession
		repoErr     error
		wantLen     int
		wantErr     bool
		errContains string
	}{
		{
			name: "returns sessions successfully",
			repoResult: []*storage.ChatSession{
				{ID: "s1", Title: "First", CreatedAt: now},
				{ID: "s2", Title: "Second", CreatedAt: now},
			},
			wantLen: 2,
		},
		{
			name:       "returns empty list",
			repoResult: []*storage.ChatSession{},
			wantLen:    0,
		},
		{
			name:        "wraps repository error",
			repoErr:     errors.New("db connection lost"),
			wantErr:     true,
			errContains: "listing sessions",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			chatRepo := new(mocks.MockChatStore)
			agentRepo := new(mocks.MockAgentStore)
			svc := newTestService(chatRepo, agentRepo)

			chatRepo.On("ListSessions").Return(tc.repoResult, tc.repoErr)

			result, err := svc.ListSessions(context.Background())

			if tc.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tc.errContains)
				assert.Nil(t, result)
			} else {
				assert.NoError(t, err)
				assert.Len(t, result, tc.wantLen)
			}
			chatRepo.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// GetSession
// ---------------------------------------------------------------------------

func TestGetSession(t *testing.T) {
	now := time.Now().UTC()

	tests := []struct {
		name        string
		sessionID   string
		repoResult  *storage.ChatSession
		repoErr     error
		wantID      string
		wantErr     bool
		errContains string
	}{
		{
			name:       "returns session successfully",
			sessionID:  "sess-abc",
			repoResult: &storage.ChatSession{ID: "sess-abc", Title: "Hello", CreatedAt: now},
			wantID:     "sess-abc",
		},
		{
			name:       "returns nil when not found",
			sessionID:  "nonexistent",
			repoResult: nil,
			repoErr:    nil,
		},
		{
			name:        "wraps repository error",
			sessionID:   "sess-fail",
			repoErr:     errors.New("read error"),
			wantErr:     true,
			errContains: "getting session",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			chatRepo := new(mocks.MockChatStore)
			agentRepo := new(mocks.MockAgentStore)
			svc := newTestService(chatRepo, agentRepo)

			chatRepo.On("GetSession", tc.sessionID).Return(tc.repoResult, tc.repoErr)

			result, err := svc.GetSession(context.Background(), tc.sessionID)

			if tc.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tc.errContains)
				assert.Nil(t, result)
			} else {
				assert.NoError(t, err)
				if tc.wantID != "" {
					assert.Equal(t, tc.wantID, result.ID)
				} else {
					assert.Nil(t, result)
				}
			}
			chatRepo.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// GetSessionWithMessages
// ---------------------------------------------------------------------------

func TestGetSessionWithMessages(t *testing.T) {
	now := time.Now().UTC()

	tests := []struct {
		name        string
		sessionID   string
		repoSession *storage.ChatSession
		repoMsgs    []storage.ChatMessage
		repoErr     error
		wantMsgLen  int
		wantErr     bool
		errContains string
	}{
		{
			name:        "returns session with messages",
			sessionID:   "sess-1",
			repoSession: &storage.ChatSession{ID: "sess-1"},
			repoMsgs: []storage.ChatMessage{
				{Role: "user", Content: "hi", Timestamp: now},
				{Role: "assistant", Content: "hello", Timestamp: now},
			},
			wantMsgLen: 2,
		},
		{
			name:        "returns session with no messages",
			sessionID:   "sess-empty",
			repoSession: &storage.ChatSession{ID: "sess-empty"},
			repoMsgs:    []storage.ChatMessage{},
			wantMsgLen:  0,
		},
		{
			name:        "wraps repository error",
			sessionID:   "sess-err",
			repoErr:     errors.New("corrupt data"),
			wantErr:     true,
			errContains: "getting session with messages",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			chatRepo := new(mocks.MockChatStore)
			agentRepo := new(mocks.MockAgentStore)
			svc := newTestService(chatRepo, agentRepo)

			chatRepo.On("GetSessionWithMessages", tc.sessionID).Return(tc.repoSession, tc.repoMsgs, tc.repoErr)

			session, msgs, err := svc.GetSessionWithMessages(context.Background(), tc.sessionID)

			if tc.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tc.errContains)
				assert.Nil(t, session)
				assert.Nil(t, msgs)
			} else {
				assert.NoError(t, err)
				assert.NotNil(t, session)
				assert.Len(t, msgs, tc.wantMsgLen)
			}
			chatRepo.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// CreateSession
// ---------------------------------------------------------------------------

func TestCreateSession(t *testing.T) {
	now := time.Now().UTC()

	tests := []struct {
		name              string
		agentSlug         string
		workingDir        string
		model             string
		settingsProfileID string
		// agent repo expectations
		agentGetResult *config.AgentConfig
		agentGetErr    error
		// chat repo expectations
		chatCreateResult *storage.ChatSession
		chatCreateErr    error
		// assertions
		wantErr     bool
		errContains string
		isNotFound  bool
	}{
		{
			name:             "creates session without agent",
			agentSlug:        "",
			workingDir:       "/home/user",
			model:            "claude-sonnet-4-20250514",
			chatCreateResult: &storage.ChatSession{ID: "new-sess", CreatedAt: now},
		},
		{
			name:             "creates session with valid agent",
			agentSlug:        "my-agent",
			workingDir:       "/workspace",
			model:            "claude-sonnet-4-20250514",
			agentGetResult:   &config.AgentConfig{Slug: "my-agent", Name: "My Agent"},
			chatCreateResult: &storage.ChatSession{ID: "agent-sess", AgentSlug: "my-agent", CreatedAt: now},
		},
		{
			name:              "creates session with settings profile",
			agentSlug:         "",
			workingDir:        "/home",
			model:             "claude-sonnet-4-20250514",
			settingsProfileID: "profile-1",
			chatCreateResult:  &storage.ChatSession{ID: "prof-sess", SettingsProfileID: "profile-1", CreatedAt: now},
		},
		{
			name:           "returns NotFoundError when agent does not exist",
			agentSlug:      "missing-agent",
			agentGetResult: nil,
			agentGetErr:    nil,
			wantErr:        true,
			isNotFound:     true,
			errContains:    "not found",
		},
		{
			name:        "wraps agent repo error",
			agentSlug:   "broken-agent",
			agentGetErr: errors.New("agent db error"),
			wantErr:     true,
			errContains: "looking up agent",
		},
		{
			name:          "wraps chat repo creation error",
			agentSlug:     "",
			chatCreateErr: errors.New("insert failed"),
			wantErr:       true,
			errContains:   "creating session",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			chatRepo := new(mocks.MockChatStore)
			agentRepo := new(mocks.MockAgentStore)
			svc := newTestService(chatRepo, agentRepo)

			if tc.agentSlug != "" {
				agentRepo.On("Get", tc.agentSlug).Return(tc.agentGetResult, tc.agentGetErr)
			}

			// Only expect CreateSession if agent validation passes.
			if !tc.wantErr || tc.chatCreateErr != nil {
				chatRepo.On("CreateSession", tc.agentSlug, tc.workingDir, tc.model, tc.settingsProfileID).
					Return(tc.chatCreateResult, tc.chatCreateErr)
			}

			result, err := svc.CreateSession(
				context.Background(), tc.agentSlug, tc.workingDir, tc.model, tc.settingsProfileID,
			)

			if tc.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tc.errContains)
				assert.Nil(t, result)
				if tc.isNotFound {
					var nfe *NotFoundError
					assert.True(t, errors.As(err, &nfe))
				}
			} else {
				assert.NoError(t, err)
				assert.NotNil(t, result)
				assert.NotEmpty(t, result.ID)
			}
			chatRepo.AssertExpectations(t)
			agentRepo.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// DeleteSession
// ---------------------------------------------------------------------------

func TestDeleteSession(t *testing.T) {
	tests := []struct {
		name        string
		sessionID   string
		repoErr     error
		wantErr     bool
		errContains string
	}{
		{
			name:      "deletes session successfully",
			sessionID: "sess-del",
		},
		{
			name:        "wraps repository error",
			sessionID:   "sess-fail",
			repoErr:     errors.New("foreign key constraint"),
			wantErr:     true,
			errContains: "deleting session",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			chatRepo := new(mocks.MockChatStore)
			agentRepo := new(mocks.MockAgentStore)
			svc := newTestService(chatRepo, agentRepo)

			chatRepo.On("DeleteSession", tc.sessionID).Return(tc.repoErr)

			err := svc.DeleteSession(context.Background(), tc.sessionID)

			if tc.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tc.errContains)
			} else {
				assert.NoError(t, err)
			}
			chatRepo.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// CommitMessage
// ---------------------------------------------------------------------------

func TestCommitMessage(t *testing.T) {
	tests := []struct {
		name           string
		session        *storage.ChatSession
		assistantText  string
		sdkSessionID   string
		isFirstMessage bool
		blocks         []storage.MessageBlock
		usage          agent.UsageStats
		// mock behavior
		appendErr error
		updateErr error
		// assertions
		wantErr         bool
		errContains     string
		expectAppend    bool
		wantInputTok    int
		wantOutputTok   int
		wantCacheCreate int
		wantCacheRead   int
	}{
		{
			name:          "commits message with text and usage",
			session:       &storage.ChatSession{ID: "s1"},
			assistantText: "Here is the answer.",
			sdkSessionID:  "sdk-123",
			blocks: []storage.MessageBlock{
				{Type: "text", Text: "Here is the answer."},
			},
			usage: agent.UsageStats{
				InputTokens:              100,
				OutputTokens:             50,
				CacheCreationInputTokens: 10,
				CacheReadInputTokens:     5,
			},
			expectAppend:    true,
			wantInputTok:    100,
			wantOutputTok:   50,
			wantCacheCreate: 10,
			wantCacheRead:   5,
		},
		{
			name: "accumulates tokens on existing totals",
			session: &storage.ChatSession{
				ID:                       "s2",
				TotalInputTokens:         200,
				TotalOutputTokens:        100,
				TotalCacheCreationTokens: 20,
				TotalCacheReadTokens:     10,
			},
			assistantText: "More text.",
			sdkSessionID:  "sdk-456",
			blocks:        []storage.MessageBlock{{Type: "text", Text: "More text."}},
			usage: agent.UsageStats{
				InputTokens:              50,
				OutputTokens:             30,
				CacheCreationInputTokens: 5,
				CacheReadInputTokens:     3,
			},
			expectAppend:    true,
			wantInputTok:    250,
			wantOutputTok:   130,
			wantCacheCreate: 25,
			wantCacheRead:   13,
		},
		{
			name:          "skips AppendMessage when assistantText is empty",
			session:       &storage.ChatSession{ID: "s3"},
			assistantText: "",
			sdkSessionID:  "sdk-789",
			blocks:        nil,
			usage:         agent.UsageStats{InputTokens: 10, OutputTokens: 5},
			expectAppend:  false,
			wantInputTok:  10,
			wantOutputTok: 5,
		},
		{
			name:          "wraps AppendMessage error",
			session:       &storage.ChatSession{ID: "s4"},
			assistantText: "fail text",
			sdkSessionID:  "sdk-err",
			blocks:        nil,
			usage:         agent.UsageStats{},
			appendErr:     errors.New("write error"),
			expectAppend:  true,
			wantErr:       true,
			errContains:   "storing assistant message",
		},
		{
			name:          "wraps UpdateSession error",
			session:       &storage.ChatSession{ID: "s5"},
			assistantText: "ok text",
			sdkSessionID:  "sdk-upd",
			blocks:        nil,
			usage:         agent.UsageStats{},
			updateErr:     errors.New("update failed"),
			expectAppend:  true,
			wantErr:       true,
			errContains:   "updating session",
		},
		{
			name:          "commits with thinking blocks",
			session:       &storage.ChatSession{ID: "s6"},
			assistantText: "Answer after thinking.",
			sdkSessionID:  "sdk-think",
			blocks: []storage.MessageBlock{
				{Type: "thinking", Text: "Let me think..."},
				{Type: "text", Text: "Answer after thinking."},
			},
			usage:         agent.UsageStats{InputTokens: 200, OutputTokens: 150},
			expectAppend:  true,
			wantInputTok:  200,
			wantOutputTok: 150,
		},
		{
			name:          "commits with zero usage",
			session:       &storage.ChatSession{ID: "s7"},
			assistantText: "Zero usage response.",
			sdkSessionID:  "sdk-zero",
			blocks:        []storage.MessageBlock{{Type: "text", Text: "Zero usage response."}},
			usage:         agent.UsageStats{},
			expectAppend:  true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			chatRepo := new(mocks.MockChatStore)
			agentRepo := new(mocks.MockAgentStore)
			svc := newTestService(chatRepo, agentRepo)

			if tc.expectAppend {
				chatRepo.On("AppendMessage", tc.session.ID, mock.MatchedBy(func(msg storage.ChatMessage) bool {
					return msg.Role == "assistant" && msg.Content == tc.assistantText
				})).Return(tc.appendErr)
			}

			// UpdateSession should only be called if AppendMessage succeeded (or was skipped).
			if tc.appendErr == nil {
				chatRepo.On("UpdateSession", mock.MatchedBy(func(s *storage.ChatSession) bool {
					return s.ID == tc.session.ID && s.SDKSession == tc.sdkSessionID
				})).Return(tc.updateErr)
			}

			err := svc.CommitMessage(
				context.Background(), tc.session,
				tc.assistantText, tc.sdkSessionID, tc.isFirstMessage,
				tc.blocks, tc.usage,
			)

			if tc.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tc.errContains)
			} else {
				assert.NoError(t, err)
				// Verify token accumulation on the session object.
				assert.Equal(t, tc.wantInputTok, tc.session.TotalInputTokens)
				assert.Equal(t, tc.wantOutputTok, tc.session.TotalOutputTokens)
				assert.Equal(t, tc.wantCacheCreate, tc.session.TotalCacheCreationTokens)
				assert.Equal(t, tc.wantCacheRead, tc.session.TotalCacheReadTokens)
				// Verify SDKSession was set.
				assert.Equal(t, tc.sdkSessionID, tc.session.SDKSession)
				// Verify UpdatedAt was set recently.
				assert.WithinDuration(t, time.Now().UTC(), tc.session.UpdatedAt, 2*time.Second)
			}

			chatRepo.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// UpdateSession
// ---------------------------------------------------------------------------

func TestUpdateSession(t *testing.T) {
	tests := []struct {
		name        string
		session     *storage.ChatSession
		repoErr     error
		wantErr     bool
		errContains string
	}{
		{
			name:    "updates session successfully",
			session: &storage.ChatSession{ID: "sess-upd", Title: "Updated"},
		},
		{
			name:        "wraps repository error",
			session:     &storage.ChatSession{ID: "sess-fail"},
			repoErr:     errors.New("db locked"),
			wantErr:     true,
			errContains: "updating session",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			chatRepo := new(mocks.MockChatStore)
			agentRepo := new(mocks.MockAgentStore)
			svc := newTestService(chatRepo, agentRepo)

			chatRepo.On("UpdateSession", mock.MatchedBy(func(s *storage.ChatSession) bool {
				return s.ID == tc.session.ID
			})).Return(tc.repoErr)

			err := svc.UpdateSession(context.Background(), tc.session)

			if tc.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tc.errContains)
			} else {
				assert.NoError(t, err)
				// Verify UpdatedAt was set recently.
				assert.WithinDuration(t, time.Now().UTC(), tc.session.UpdatedAt, 2*time.Second)
			}
			chatRepo.AssertExpectations(t)
		})
	}
}
