package telegram

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/messaging"
)

func TestSplitMessage(t *testing.T) {
	tests := []struct {
		name   string
		text   string
		maxLen int
		want   int // expected number of chunks
	}{
		{"short message", "hello", 4096, 1},
		{"exactly at limit", strings.Repeat("a", 4096), 4096, 1},
		{"over limit", strings.Repeat("a", 5000), 4096, 2},
		{"with newline boundary", "line1\nline2\n" + strings.Repeat("a", 4090), 4096, 2},
		{"empty message", "", 4096, 1},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			chunks := splitMessage(tt.text, tt.maxLen)
			if len(chunks) != tt.want {
				t.Errorf("got %d chunks, want %d", len(chunks), tt.want)
			}
			// Verify reassembled content matches original.
			reassembled := strings.Join(chunks, "")
			if reassembled != tt.text {
				t.Error("reassembled text does not match original")
			}
			// Verify no chunk exceeds maxLen.
			for i, chunk := range chunks {
				if len(chunk) > tt.maxLen {
					t.Errorf("chunk %d exceeds max length: %d > %d", i, len(chunk), tt.maxLen)
				}
			}
		})
	}
}

func TestAdapterHandleWebhook(t *testing.T) {
	var mu sync.Mutex
	var received []messaging.InboundMessage

	handler := messaging.MessageHandlerFunc(func(_ context.Context, msg messaging.InboundMessage) error {
		mu.Lock()
		defer mu.Unlock()
		received = append(received, msg)
		return nil
	})

	adapter := NewAdapter("integration-1", "test-token", handler, nil)
	// Use a no-op logger to avoid nil pointer.
	adapter.logger = newTestLogger()

	update := telegramUpdate{
		UpdateID: 12345,
		Message: &telegramMsg{
			MessageID: 42,
			From: &telegramUser{
				ID:        100,
				FirstName: "John",
				LastName:  "Doe",
				Username:  "johndoe",
			},
			Chat: telegramChat{
				ID:   9876,
				Type: "private",
			},
			Date: time.Now().Unix(),
			Text: "Hello bot!",
		},
	}

	body, _ := json.Marshal(update)
	req := httptest.NewRequest(http.MethodPost, "/webhook", strings.NewReader(string(body)))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	adapter.HandleWebhook(w, req)

	if w.Code != http.StatusOK {
		t.Errorf("got status %d, want %d", w.Code, http.StatusOK)
	}

	// Wait a bit for the async handler goroutine.
	time.Sleep(100 * time.Millisecond)

	mu.Lock()
	defer mu.Unlock()
	if len(received) != 1 {
		t.Fatalf("got %d messages, want 1", len(received))
	}

	msg := received[0]
	if msg.PlatformType != "telegram" {
		t.Errorf("got platform type %q, want %q", msg.PlatformType, "telegram")
	}
	if msg.PlatformID != "integration-1" {
		t.Errorf("got platform ID %q, want %q", msg.PlatformID, "integration-1")
	}
	if msg.ChannelID != "9876" {
		t.Errorf("got channel ID %q, want %q", msg.ChannelID, "9876")
	}
	if msg.UserID != "100" {
		t.Errorf("got user ID %q, want %q", msg.UserID, "100")
	}
	if msg.UserDisplayName != "John Doe" {
		t.Errorf("got display name %q, want %q", msg.UserDisplayName, "John Doe")
	}
	if msg.Content != "Hello bot!" {
		t.Errorf("got content %q, want %q", msg.Content, "Hello bot!")
	}
}

func TestAdapterHandleWebhookMethodNotAllowed(t *testing.T) {
	adapter := NewAdapter("test", "token", nil, newTestLogger())

	req := httptest.NewRequest(http.MethodGet, "/webhook", nil)
	w := httptest.NewRecorder()

	adapter.HandleWebhook(w, req)

	if w.Code != http.StatusMethodNotAllowed {
		t.Errorf("got status %d, want %d", w.Code, http.StatusMethodNotAllowed)
	}
}

func TestAdapterHandleWebhookEmptyMessage(t *testing.T) {
	called := false
	handler := messaging.MessageHandlerFunc(func(_ context.Context, _ messaging.InboundMessage) error {
		called = true
		return nil
	})

	adapter := NewAdapter("test", "token", handler, newTestLogger())

	// Update with no message.
	update := telegramUpdate{UpdateID: 1}
	body, _ := json.Marshal(update)
	req := httptest.NewRequest(http.MethodPost, "/webhook", strings.NewReader(string(body)))
	w := httptest.NewRecorder()

	adapter.HandleWebhook(w, req)

	time.Sleep(50 * time.Millisecond)
	if called {
		t.Error("handler should not be called for empty message")
	}
}

func TestAdapterSendMessage(t *testing.T) {
	var receivedBody map[string]any

	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		json.Unmarshal(body, &receivedBody) //nolint:errcheck
		w.WriteHeader(http.StatusOK)
		json.NewEncoder(w).Encode(map[string]any{"ok": true}) //nolint:errcheck
	}))
	defer ts.Close()

	// Override the API base URL.
	origURL := apiBaseURL
	apiBaseURL = ts.URL
	defer func() { apiBaseURL = origURL }()

	adapter := NewAdapter("test", "fake-token", nil, newTestLogger())

	err := adapter.SendMessage(context.Background(), messaging.OutboundMessage{
		ChannelID:        "12345",
		Content:          "Hello!",
		ReplyToMessageID: "42",
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if receivedBody == nil {
		t.Fatal("no request body received")
	}
	if receivedBody["text"] != "Hello!" {
		t.Errorf("got text %v, want %q", receivedBody["text"], "Hello!")
	}
	if receivedBody["parse_mode"] != "Markdown" {
		t.Errorf("got parse_mode %v, want %q", receivedBody["parse_mode"], "Markdown")
	}
}

func TestAdapterSendTypingIndicator(t *testing.T) {
	var receivedBody map[string]any

	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		json.Unmarshal(body, &receivedBody) //nolint:errcheck
		w.WriteHeader(http.StatusOK)
		json.NewEncoder(w).Encode(map[string]any{"ok": true}) //nolint:errcheck
	}))
	defer ts.Close()

	origURL := apiBaseURL
	apiBaseURL = ts.URL
	defer func() { apiBaseURL = origURL }()

	adapter := NewAdapter("test", "fake-token", nil, newTestLogger())

	err := adapter.SendTypingIndicator(context.Background(), "12345")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if receivedBody["action"] != "typing" {
		t.Errorf("got action %v, want %q", receivedBody["action"], "typing")
	}
}

func TestNewFactory(t *testing.T) {
	factory := NewFactory(newTestLogger())

	handler := messaging.MessageHandlerFunc(func(_ context.Context, _ messaging.InboundMessage) error {
		return nil
	})

	// Missing bot_token should error.
	_, err := factory("id", map[string]string{}, handler)
	if err == nil {
		t.Error("expected error for missing bot_token")
	}

	// Valid credentials should work.
	p, err := factory("id", map[string]string{"bot_token": "123:ABC"}, handler)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if p.ID() != "id" {
		t.Errorf("got ID %q, want %q", p.ID(), "id")
	}
	if p.Type() != "telegram" {
		t.Errorf("got Type %q, want %q", p.Type(), "telegram")
	}
}

// newTestLogger returns a slog.Logger suitable for tests.
func newTestLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}
