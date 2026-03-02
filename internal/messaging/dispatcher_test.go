package messaging

import (
	"context"
	"net/http"
	"testing"
)

func TestPlatformRegistry(t *testing.T) {
	reg := newPlatformRegistry()

	// Test empty get.
	_, ok := reg.get("nonexistent")
	if ok {
		t.Error("expected get to return false for nonexistent key")
	}

	// Test put and get.
	mock := &mockPlatform{id: "test-1", typeName: "telegram"}
	reg.put("test-1", mock)

	p, ok := reg.get("test-1")
	if !ok {
		t.Error("expected get to return true after put")
	}
	if p.ID() != "test-1" {
		t.Errorf("got ID %q, want %q", p.ID(), "test-1")
	}
	if p.Type() != "telegram" {
		t.Errorf("got Type %q, want %q", p.Type(), "telegram")
	}

	// Test all.
	mock2 := &mockPlatform{id: "test-2", typeName: "slack"}
	reg.put("test-2", mock2)

	all := reg.all()
	if len(all) != 2 {
		t.Errorf("got %d platforms, want 2", len(all))
	}

	// Test delete.
	reg.delete("test-1")
	_, ok = reg.get("test-1")
	if ok {
		t.Error("expected get to return false after delete")
	}

	all = reg.all()
	if len(all) != 1 {
		t.Errorf("got %d platforms after delete, want 1", len(all))
	}
}

func TestMessageHandlerFunc(t *testing.T) {
	called := false
	handler := MessageHandlerFunc(func(_ context.Context, msg InboundMessage) error {
		called = true
		if msg.Content != "hello" {
			t.Errorf("got content %q, want %q", msg.Content, "hello")
		}
		return nil
	})

	err := handler.HandleMessage(context.Background(), InboundMessage{Content: "hello"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !called {
		t.Error("handler was not called")
	}
}

// mockPlatform is a minimal Platform implementation for testing.
type mockPlatform struct {
	id            string
	typeName      string
	lastMessage   OutboundMessage
	sendCallCount int
}

func (m *mockPlatform) ID() string                                     { return m.id }
func (m *mockPlatform) Type() string                                   { return m.typeName }
func (m *mockPlatform) Start(_ context.Context) error                  { return nil }
func (m *mockPlatform) Stop() error                                    { return nil }
func (m *mockPlatform) HandleWebhook(_ http.ResponseWriter, _ *http.Request) {}
func (m *mockPlatform) SendTypingIndicator(_ context.Context, _ string) error {
	return nil
}
func (m *mockPlatform) SendMessage(_ context.Context, msg OutboundMessage) error {
	m.lastMessage = msg
	m.sendCallCount++
	return nil
}
