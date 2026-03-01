package api

import (
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"
	"github.com/stretchr/testify/assert"

	"github.com/shaharia-lab/agento/internal/agent"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/service"
	"github.com/shaharia-lab/agento/internal/storage"
)

func TestTruncateTitle(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		maxRunes int
		expected string
	}{
		{
			name:     "short string under limit",
			input:    "hello",
			maxRunes: 10,
			expected: "hello",
		},
		{
			name:     "exact limit",
			input:    "hello",
			maxRunes: 5,
			expected: "hello",
		},
		{
			name:     "over limit truncated with ellipsis",
			input:    "hello world this is a long title",
			maxRunes: 10,
			expected: "hello worl...",
		},
		{
			name:     "empty string",
			input:    "",
			maxRunes: 10,
			expected: "",
		},
		{
			name:     "unicode characters multi-byte",
			input:    "こんにちは世界のテスト",
			maxRunes: 5,
			expected: "こんにちは...",
		},
		{
			name:     "unicode under limit",
			input:    "日本語",
			maxRunes: 5,
			expected: "日本語",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := truncateTitle(tt.input, tt.maxRunes)
			assert.Equal(t, tt.expected, result)
		})
	}
}

func TestAppendAssistantBlocks(t *testing.T) {
	tests := []struct {
		name     string
		initial  []storage.MessageBlock
		raw      json.RawMessage
		expected []storage.MessageBlock
	}{
		{
			name:    "valid thinking block",
			initial: nil,
			raw:     json.RawMessage(`{"message":{"content":[{"type":"thinking","thinking":"let me think"}]}}`),
			expected: []storage.MessageBlock{
				{Type: "thinking", Text: "let me think"},
			},
		},
		{
			name:    "valid text block",
			initial: nil,
			raw:     json.RawMessage(`{"message":{"content":[{"type":"text","text":"hello world"}]}}`),
			expected: []storage.MessageBlock{
				{Type: "text", Text: "hello world"},
			},
		},
		{
			name:    "valid tool_use block",
			initial: nil,
			raw:     json.RawMessage(`{"message":{"content":[{"type":"tool_use","id":"t1","name":"my_tool","input":{"key":"val"}}]}}`),
			expected: []storage.MessageBlock{
				{Type: "tool_use", ID: "t1", Name: "my_tool", Input: json.RawMessage(`{"key":"val"}`)},
			},
		},
		{
			name:    "multiple blocks in one event",
			initial: nil,
			raw: json.RawMessage(`{"message":{"content":[
				{"type":"thinking","thinking":"step 1"},
				{"type":"text","text":"result"},
				{"type":"tool_use","id":"t2","name":"bash","input":{}}
			]}}`),
			expected: []storage.MessageBlock{
				{Type: "thinking", Text: "step 1"},
				{Type: "text", Text: "result"},
				{Type: "tool_use", ID: "t2", Name: "bash", Input: json.RawMessage(`{}`)},
			},
		},
		{
			name:     "empty thinking skipped",
			initial:  nil,
			raw:      json.RawMessage(`{"message":{"content":[{"type":"thinking","thinking":""}]}}`),
			expected: nil,
		},
		{
			name:     "empty text skipped",
			initial:  nil,
			raw:      json.RawMessage(`{"message":{"content":[{"type":"text","text":""}]}}`),
			expected: nil,
		},
		{
			name:     "invalid JSON returns original blocks",
			initial:  []storage.MessageBlock{{Type: "text", Text: "existing"}},
			raw:      json.RawMessage(`not valid json`),
			expected: []storage.MessageBlock{{Type: "text", Text: "existing"}},
		},
		{
			name:    "nil initial blocks",
			initial: nil,
			raw:     json.RawMessage(`{"message":{"content":[{"type":"text","text":"new"}]}}`),
			expected: []storage.MessageBlock{
				{Type: "text", Text: "new"},
			},
		},
		{
			name:    "appends to existing blocks",
			initial: []storage.MessageBlock{{Type: "text", Text: "first"}},
			raw:     json.RawMessage(`{"message":{"content":[{"type":"text","text":"second"}]}}`),
			expected: []storage.MessageBlock{
				{Type: "text", Text: "first"},
				{Type: "text", Text: "second"},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := appendAssistantBlocks(tt.initial, tt.raw)
			assert.Equal(t, tt.expected, result)
		})
	}
}

func TestExtractAskUserQuestionInput(t *testing.T) {
	tests := []struct {
		name     string
		raw      json.RawMessage
		expected json.RawMessage
	}{
		{
			name:     "valid AskUserQuestion tool_use returns input",
			raw:      json.RawMessage(`{"message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"question":"what?"}}]}}`),
			expected: json.RawMessage(`{"question":"what?"}`),
		},
		{
			name:     "different tool name returns nil",
			raw:      json.RawMessage(`{"message":{"content":[{"type":"tool_use","name":"SomeOtherTool","input":{"x":1}}]}}`),
			expected: nil,
		},
		{
			name:     "no tool_use blocks returns nil",
			raw:      json.RawMessage(`{"message":{"content":[]}}`),
			expected: nil,
		},
		{
			name:     "text block only returns nil",
			raw:      json.RawMessage(`{"message":{"content":[{"type":"text","text":"hello"}]}}`),
			expected: nil,
		},
		{
			name:     "invalid JSON returns nil",
			raw:      json.RawMessage(`{bad json`),
			expected: nil,
		},
		{
			name: "multiple tool_uses AskUserQuestion is second",
			raw: json.RawMessage(`{"message":{"content":[
				{"type":"tool_use","name":"Bash","input":{"cmd":"ls"}},
				{"type":"tool_use","name":"AskUserQuestion","input":{"q":"confirm?"}}
			]}}`),
			expected: json.RawMessage(`{"q":"confirm?"}`),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := extractAskUserQuestionInput(tt.raw)
			if tt.expected == nil {
				assert.Nil(t, result)
			} else {
				assert.JSONEq(t, string(tt.expected), string(result))
			}
		})
	}
}

func TestTokenAccumulator(t *testing.T) {
	tests := []struct {
		name     string
		results  []*claude.Result
		expected agent.UsageStats
	}{
		{
			name:    "add nil result is no-op",
			results: []*claude.Result{nil},
			expected: agent.UsageStats{
				InputTokens:              0,
				OutputTokens:             0,
				CacheCreationInputTokens: 0,
				CacheReadInputTokens:     0,
			},
		},
		{
			name: "add single result",
			results: []*claude.Result{
				{
					Usage: claude.Usage{
						InputTokens:              100,
						OutputTokens:             50,
						CacheCreationInputTokens: 10,
						CacheReadInputTokens:     20,
					},
				},
			},
			expected: agent.UsageStats{
				InputTokens:              100,
				OutputTokens:             50,
				CacheCreationInputTokens: 10,
				CacheReadInputTokens:     20,
			},
		},
		{
			name: "add multiple results accumulates",
			results: []*claude.Result{
				{
					Usage: claude.Usage{
						InputTokens:              100,
						OutputTokens:             50,
						CacheCreationInputTokens: 10,
						CacheReadInputTokens:     20,
					},
				},
				{
					Usage: claude.Usage{
						InputTokens:              200,
						OutputTokens:             75,
						CacheCreationInputTokens: 5,
						CacheReadInputTokens:     30,
					},
				},
			},
			expected: agent.UsageStats{
				InputTokens:              300,
				OutputTokens:             125,
				CacheCreationInputTokens: 15,
				CacheReadInputTokens:     50,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var acc tokenAccumulator
			for _, r := range tt.results {
				acc.add(r)
			}
			stats := acc.toUsageStats()
			assert.Equal(t, tt.expected, stats)
		})
	}
}

// flusherRecorder implements http.ResponseWriter and http.Flusher for testing SSE.
type flusherRecorder struct {
	*httptest.ResponseRecorder
	flushed bool
}

func (f *flusherRecorder) Flush() {
	f.flushed = true
}

func TestSendSSERaw(t *testing.T) {
	tests := []struct {
		name     string
		event    string
		raw      json.RawMessage
		expected string
	}{
		{
			name:     "valid event written to response",
			event:    "assistant",
			raw:      json.RawMessage(`{"message":"hello"}`),
			expected: "event: assistant\ndata: {\"message\":\"hello\"}\n\n",
		},
		{
			name:     "result event",
			event:    "result",
			raw:      json.RawMessage(`{"ok":true}`),
			expected: "event: result\ndata: {\"ok\":true}\n\n",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := httptest.NewRecorder()
			f := &flusherRecorder{ResponseRecorder: rec}

			sendSSERaw(f, f, tt.event, tt.raw)

			assert.Equal(t, tt.expected, rec.Body.String())
			assert.True(t, f.flushed, "Flush should have been called")
		})
	}
}

func TestHttpErr(t *testing.T) {
	tests := []struct {
		name           string
		err            error
		expectedStatus int
	}{
		{
			name:           "NotFoundError maps to 404",
			err:            &service.NotFoundError{Resource: "agent", ID: "abc"},
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "ValidationError maps to 400",
			err:            &service.ValidationError{Field: "name", Message: "required"},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "ConflictError maps to 409",
			err:            &service.ConflictError{Resource: "agent", ID: "abc"},
			expectedStatus: http.StatusConflict,
		},
		{
			name:           "generic error maps to 500",
			err:            errors.New("something went wrong"),
			expectedStatus: http.StatusInternalServerError,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := httptest.NewRecorder()
			httpErr(rec, tt.err)

			assert.Equal(t, tt.expectedStatus, rec.Code)

			var body map[string]string
			err := json.Unmarshal(rec.Body.Bytes(), &body)
			assert.NoError(t, err)
			assert.Contains(t, body, "error")
			assert.NotEmpty(t, body["error"])
		})
	}
}

func TestScrubIntegration(t *testing.T) {
	now := time.Now()

	tests := []struct {
		name              string
		cfg               *config.IntegrationConfig
		expectAuth        bool
		expectKeysMissing []string
	}{
		{
			name: "authenticated is true when Auth is not nil",
			cfg: &config.IntegrationConfig{
				ID:          "int-1",
				Name:        "My Google",
				Type:        "google",
				Enabled:     true,
				Credentials: json.RawMessage(`{"client_id":"secret-client-id","client_secret":"secret-client-secret"}`),
				Auth:        json.RawMessage(`{"access_token":"secret-access-token","refresh_token":"secret-refresh-token"}`),
				Services: map[string]config.ServiceConfig{
					"calendar": {Enabled: true, Tools: []string{"list_events"}},
				},
				CreatedAt: now,
				UpdatedAt: now,
			},
			expectAuth:        true,
			expectKeysMissing: []string{"credentials", "auth"},
		},
		{
			name: "authenticated is false when Auth is nil",
			cfg: &config.IntegrationConfig{
				ID:          "int-2",
				Name:        "Unauthenticated",
				Type:        "google",
				Enabled:     false,
				Credentials: json.RawMessage(`{"client_id":"client-id","client_secret":"client-secret"}`),
				Auth:        nil,
				Services:    map[string]config.ServiceConfig{},
				CreatedAt:   now,
				UpdatedAt:   now,
			},
			expectAuth:        false,
			expectKeysMissing: []string{"credentials", "auth"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := scrubIntegration(tt.cfg)

			assert.Equal(t, tt.cfg.ID, result["id"])
			assert.Equal(t, tt.cfg.Name, result["name"])
			assert.Equal(t, tt.cfg.Type, result["type"])
			assert.Equal(t, tt.cfg.Enabled, result["enabled"])
			assert.Equal(t, tt.expectAuth, result["authenticated"])
			assert.Equal(t, tt.cfg.Services, result["services"])
			assert.Equal(t, tt.cfg.CreatedAt, result["created_at"])
			assert.Equal(t, tt.cfg.UpdatedAt, result["updated_at"])

			for _, key := range tt.expectKeysMissing {
				_, exists := result[key]
				assert.False(t, exists, "key %q should not be present in scrubbed output", key)
			}
		})
	}
}
