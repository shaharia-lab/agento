package telegram

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// setupTestServer creates an httptest.Server and points apiBaseURL at it.
// The caller must call the returned cleanup function when done.
func setupTestServer(handler http.HandlerFunc) (*httptest.Server, func()) {
	srv := httptest.NewServer(handler)
	origBase := apiBaseURL
	apiBaseURL = srv.URL
	return srv, func() {
		srv.Close()
		apiBaseURL = origBase
	}
}

// writeTestJSON is a test helper that writes a JSON-encoded value to w.
func writeTestJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

func TestCallTelegram(t *testing.T) {
	tests := []struct {
		name       string
		handler    http.HandlerFunc
		wantErr    bool
		wantErrMsg string
	}{
		{
			name: "success",
			handler: func(w http.ResponseWriter, _ *http.Request) {
				writeTestJSON(w, telegramResponse{
					OK:     true,
					Result: json.RawMessage(`{"message_id":42}`),
				})
			},
		},
		{
			name: "api error (ok=false)",
			handler: func(w http.ResponseWriter, _ *http.Request) {
				writeTestJSON(w, telegramResponse{
					OK:          false,
					Description: "Bad Request: chat not found",
				})
			},
			wantErr:    true,
			wantErrMsg: "telegram API error: Bad Request: chat not found",
		},
		{
			name: "json parse failure",
			handler: func(w http.ResponseWriter, _ *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				fmt.Fprint(w, "not json")
			},
			wantErr:    true,
			wantErrMsg: "parsing response:",
		},
		{
			name: "network error",
			handler: func(_ http.ResponseWriter, _ *http.Request) {
				// handler is unused; we close the server before calling
			},
			wantErr:    true,
			wantErrMsg: "request failed",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			srv, cleanup := setupTestServer(tc.handler)

			// For network error test, close the server before making the call.
			if tc.name == "network error" {
				srv.Close()
			}

			resp, err := callTelegram(context.Background(), "test-token", "sendMessage", map[string]any{"chat_id": "123"})

			if tc.wantErr {
				if err == nil {
					t.Fatal("expected error, got nil")
				}
				if tc.wantErrMsg != "" && !containsStr(err.Error(), tc.wantErrMsg) {
					t.Errorf("error %q does not contain %q", err.Error(), tc.wantErrMsg)
				}
			} else {
				if err != nil {
					t.Fatalf("unexpected error: %v", err)
				}
				if resp == nil {
					t.Fatal("expected non-nil response")
				}
				if !resp.OK {
					t.Error("expected resp.OK to be true")
				}
			}

			// Only call cleanup if we haven't already closed the server.
			if tc.name == "network error" {
				apiBaseURL = "https://api.telegram.org"
			} else {
				cleanup()
			}
		})
	}
}

func TestValidateBotToken(t *testing.T) {
	tests := []struct {
		name         string
		handler      http.HandlerFunc
		wantUsername string
		wantErr      bool
		wantErrMsg   string
	}{
		{
			name: "valid token returns username",
			handler: func(w http.ResponseWriter, _ *http.Request) {
				bot := botUser{ID: 123, IsBot: true, FirstName: "TestBot", Username: "test_bot"}
				result, _ := json.Marshal(bot)
				writeTestJSON(w, telegramResponse{OK: true, Result: result})
			},
			wantUsername: "test_bot",
		},
		{
			name: "invalid token (ok=false)",
			handler: func(w http.ResponseWriter, _ *http.Request) {
				writeTestJSON(w, telegramResponse{
					OK:          false,
					Description: "Unauthorized",
				})
			},
			wantErr:    true,
			wantErrMsg: "telegram API error: Unauthorized",
		},
		{
			name: "network error",
			handler: func(_ http.ResponseWriter, _ *http.Request) {
				// unused; server is closed before call
			},
			wantErr:    true,
			wantErrMsg: "request failed",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			srv, cleanup := setupTestServer(tc.handler)

			if tc.name == "network error" {
				srv.Close()
			}

			username, err := ValidateBotToken(context.Background(), "test-token")

			if tc.wantErr {
				if err == nil {
					t.Fatal("expected error, got nil")
				}
				if tc.wantErrMsg != "" && !containsStr(err.Error(), tc.wantErrMsg) {
					t.Errorf("error %q does not contain %q", err.Error(), tc.wantErrMsg)
				}
			} else {
				if err != nil {
					t.Fatalf("unexpected error: %v", err)
				}
				if username != tc.wantUsername {
					t.Errorf("got username %q, want %q", username, tc.wantUsername)
				}
			}

			if tc.name == "network error" {
				apiBaseURL = "https://api.telegram.org"
			} else {
				cleanup()
			}
		})
	}
}

func TestHandleReadMessages_LimitClamping(t *testing.T) {
	tests := []struct {
		name      string
		input     int
		wantLimit int
	}{
		{name: "zero becomes 100", input: 0, wantLimit: 100},
		{name: "over 100 becomes 100", input: 200, wantLimit: 100},
		{name: "50 stays 50", input: 50, wantLimit: 50},
		{name: "negative becomes 100", input: -5, wantLimit: 100},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			var receivedLimit int
			handler := func(w http.ResponseWriter, r *http.Request) {
				var payload map[string]any
				_ = json.NewDecoder(r.Body).Decode(&payload)
				if v, ok := payload["limit"]; ok {
					receivedLimit = int(v.(float64))
				}
				writeTestJSON(w, telegramResponse{OK: true, Result: json.RawMessage(`[]`)})
			}

			_, cleanup := setupTestServer(handler)
			defer cleanup()

			_, _, err := handleReadMessages(context.Background(), "test-token", &readMessagesParams{Limit: tc.input})
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}

			if receivedLimit != tc.wantLimit {
				t.Errorf("got limit %d, want %d", receivedLimit, tc.wantLimit)
			}
		})
	}
}

func TestHandleCreatePoll_OptionsValidation(t *testing.T) {
	tests := []struct {
		name    string
		options []string
		wantErr bool
	}{
		{name: "too few options", options: []string{"only one"}, wantErr: true},
		{name: "too many options", options: []string{"1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11"}, wantErr: true},
		{name: "valid 2 options", options: []string{"yes", "no"}, wantErr: false},
		{name: "valid 10 options", options: []string{"1", "2", "3", "4", "5", "6", "7", "8", "9", "10"}, wantErr: false},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			handler := func(w http.ResponseWriter, _ *http.Request) {
				writeTestJSON(w, telegramResponse{OK: true, Result: json.RawMessage(`{"message_id":1}`)})
			}

			_, cleanup := setupTestServer(handler)
			defer cleanup()

			_, _, err := handleCreatePoll(context.Background(), "test-token", &createPollParams{
				ChatID:   "123",
				Question: "Test?",
				Options:  tc.options,
			})

			if tc.wantErr {
				if err == nil {
					t.Fatal("expected error, got nil")
				}
				if !containsStr(err.Error(), "poll requires 2-10 options") {
					t.Errorf("unexpected error message: %v", err)
				}
			} else if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
		})
	}
}

func TestCallTelegram_DoesNotLeakToken(t *testing.T) {
	// Close the server immediately so the HTTP call fails.
	srv := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, _ *http.Request) {}))
	origBase := apiBaseURL
	apiBaseURL = srv.URL
	srv.Close()
	defer func() { apiBaseURL = origBase }()

	secretToken := "1234567890:ABCdefGHIjklMNOpqrsTUVwxyz"
	_, err := callTelegram(context.Background(), secretToken, "sendMessage", map[string]any{})
	if err == nil {
		t.Fatal("expected error, got nil")
	}

	if containsStr(err.Error(), secretToken) {
		t.Errorf("error message leaks bot token: %s", err.Error())
	}
	if !containsStr(err.Error(), "request failed") {
		t.Errorf("expected 'request failed' in error, got: %s", err.Error())
	}
}

// containsStr is a test helper to check if s contains substr.
func containsStr(s, substr string) bool {
	return strings.Contains(s, substr)
}
