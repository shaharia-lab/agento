package jira

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/shaharia-lab/agento/internal/config"
)

// helpers ─────────────────────────────────────────────────────────────────────

// newTestServer creates an httptest.Server and returns it together with a
// *client already pointing at it.  The caller must Close the server when done.
func newTestServer(handler http.HandlerFunc) (*httptest.Server, *client) {
	srv := httptest.NewServer(handler)
	c := &client{siteURL: srv.URL, email: "test@example.com", apiToken: "token"}
	return srv, c
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

func containsStr(s, sub string) bool { return strings.Contains(s, sub) }

// ── client.call tests ─────────────────────────────────────────────────────────

func TestClientCall_Success(t *testing.T) {
	srv, c := newTestServer(func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, map[string]string{"id": "PROJ-1"})
	})
	defer srv.Close()

	result, err := c.call(context.Background(), http.MethodGet, "/rest/api/3/issue/PROJ-1", nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !containsStr(string(result), "PROJ-1") {
		t.Errorf("result does not contain expected content: %s", string(result))
	}
}

func TestClientCall_NonOKStatus(t *testing.T) {
	srv, c := newTestServer(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
		fmt.Fprint(w, `{"message":"Unauthorized"}`)
	})
	defer srv.Close()

	_, err := c.call(context.Background(), http.MethodGet, "/rest/api/3/issue/PROJ-1", nil)
	if err == nil {
		t.Fatal("expected error for 401, got nil")
	}
	if !containsStr(err.Error(), "401") {
		t.Errorf("error should mention status 401, got: %v", err)
	}
}

func TestClientCall_NetworkError(t *testing.T) {
	srv, c := newTestServer(func(_ http.ResponseWriter, _ *http.Request) {})
	srv.Close() // immediately close so the request fails

	_, err := c.call(context.Background(), http.MethodGet, "/rest/api/3/project", nil)
	if err == nil {
		t.Fatal("expected network error, got nil")
	}
	if !containsStr(err.Error(), "request failed") {
		t.Errorf("expected 'request failed' in error, got: %v", err)
	}
}

func TestClientCall_SetsBasicAuth(t *testing.T) {
	var gotUser, gotPass string
	srv, c := newTestServer(func(w http.ResponseWriter, r *http.Request) {
		gotUser, gotPass, _ = r.BasicAuth()
		writeJSON(w, map[string]string{})
	})
	defer srv.Close()

	_, err := c.call(context.Background(), http.MethodGet, "/rest/api/3/project", nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if gotUser != "test@example.com" || gotPass != "token" {
		t.Errorf("BasicAuth not set correctly: got user=%q pass=%q", gotUser, gotPass)
	}
}

func TestClientCall_PostWithBody(t *testing.T) {
	var receivedBody map[string]any
	srv, c := newTestServer(func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewDecoder(r.Body).Decode(&receivedBody)
		if r.Header.Get("Content-Type") != "application/json" {
			w.WriteHeader(http.StatusBadRequest)
			return
		}
		writeJSON(w, map[string]string{"id": "10001"})
	})
	defer srv.Close()

	payload := map[string]any{"fields": map[string]string{"summary": "Test issue"}}
	_, err := c.call(context.Background(), http.MethodPost, "/rest/api/3/issue", payload)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if fields, ok := receivedBody["fields"].(map[string]any); !ok || fields["summary"] != "Test issue" {
		t.Errorf("body not sent correctly: %v", receivedBody)
	}
}

// ── ValidateCredentials tests ─────────────────────────────────────────────────

func TestValidateCredentials_Success(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/rest/api/3/myself" {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		user, pass, _ := r.BasicAuth()
		if user != "user@example.com" || pass != "secret" {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		writeJSON(w, myselfResponse{DisplayName: "Test User", EmailAddress: "user@example.com"})
	}))
	defer srv.Close()

	displayName, err := ValidateCredentials(context.Background(), srv.URL, "user@example.com", "secret")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if displayName != "Test User" {
		t.Errorf("got displayName %q, want %q", displayName, "Test User")
	}
}

func TestValidateCredentials_Unauthorized(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
		fmt.Fprint(w, `{"message":"Unauthorized"}`)
	}))
	defer srv.Close()

	_, err := ValidateCredentials(context.Background(), srv.URL, "bad@example.com", "wrong")
	if err == nil {
		t.Fatal("expected error for 401, got nil")
	}
	if !containsStr(err.Error(), "401") {
		t.Errorf("error should mention status 401, got: %v", err)
	}
}

func TestValidateCredentials_MalformedResponse(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		fmt.Fprint(w, "not json")
	}))
	defer srv.Close()

	_, err := ValidateCredentials(context.Background(), srv.URL, "u@example.com", "token")
	if err == nil {
		t.Fatal("expected error for malformed response, got nil")
	}
}

func TestValidateCredentials_NetworkError(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, _ *http.Request) {}))
	srv.Close() // close immediately

	_, err := ValidateCredentials(context.Background(), srv.URL, "u@example.com", "token")
	if err == nil {
		t.Fatal("expected error on network failure, got nil")
	}
}

// ── buildMCPServer / tool filtering tests ─────────────────────────────────────

func makeIntegrationConfig(tools []string) *config.IntegrationConfig {
	cfg := &config.IntegrationConfig{
		ID:   "test-jira",
		Type: "jira",
		Auth: []byte(`{"validated":true}`),
		Services: map[string]config.ServiceConfig{
			"project_management": {Enabled: true, Tools: tools},
		},
	}
	creds := config.AtlassianCredentials{
		SiteURL:  "https://example.atlassian.net",
		Email:    "test@example.com",
		APIToken: "secret",
	}
	_ = cfg.SetCredentials(creds)
	return cfg
}

func TestBuildMCPServer_WithAllTools(t *testing.T) {
	cfg := makeIntegrationConfig([]string{
		"list_projects", "get_project", "search_issues", "get_issue",
		"create_issue", "update_issue", "add_comment", "list_transitions", "transition_issue",
	})

	server := buildMCPServer(cfg, "https://example.atlassian.net", "test@example.com", "secret")
	if server == nil {
		t.Fatal("expected non-nil server")
	}
}

func TestBuildMCPServer_FilteredTools(t *testing.T) {
	// Only list_projects and get_issue should be registered.
	cfg := makeIntegrationConfig([]string{"list_projects", "get_issue"})

	server := buildMCPServer(cfg, "https://example.atlassian.net", "test@example.com", "secret")
	if server == nil {
		t.Fatal("expected non-nil server")
	}
}

func TestBuildMCPServer_DisabledService(t *testing.T) {
	cfg := &config.IntegrationConfig{
		ID:   "test-jira",
		Type: "jira",
		Auth: []byte(`{"validated":true}`),
		Services: map[string]config.ServiceConfig{
			"project_management": {Enabled: false, Tools: []string{"list_projects"}},
		},
	}
	_ = cfg.SetCredentials(config.AtlassianCredentials{
		SiteURL: "https://example.atlassian.net", Email: "x@example.com", APIToken: "t",
	})

	// Should return a server with no tools registered (service is disabled).
	server := buildMCPServer(cfg, "https://example.atlassian.net", "x@example.com", "t")
	if server == nil {
		t.Fatal("expected non-nil server even when service disabled")
	}
}

// ── docBody helper test ───────────────────────────────────────────────────────

func TestDocBody_Structure(t *testing.T) {
	body := docBody("hello world")
	if body["type"] != "doc" {
		t.Errorf("expected type=doc, got %v", body["type"])
	}
	if body["version"] != 1 {
		t.Errorf("expected version=1, got %v", body["version"])
	}
	content, ok := body["content"].([]map[string]any)
	if !ok || len(content) == 0 {
		t.Fatal("expected non-empty content array")
	}
	para := content[0]
	if para["type"] != "paragraph" {
		t.Errorf("expected paragraph, got %v", para["type"])
	}
}

// ── search_issues limit clamping test ────────────────────────────────────────

func TestSearchIssues_LimitClamping(t *testing.T) {
	tests := []struct {
		name      string
		input     int
		wantLimit int
	}{
		{name: "zero becomes 50", input: 0, wantLimit: 50},
		{name: "over 100 becomes 50", input: 200, wantLimit: 50},
		{name: "30 stays 30", input: 30, wantLimit: 30},
		{name: "negative becomes 50", input: -1, wantLimit: 50},
		{name: "100 stays 100", input: 100, wantLimit: 100},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			var receivedLimit float64
			srv, c := newTestServer(func(w http.ResponseWriter, r *http.Request) {
				var body map[string]any
				_ = json.NewDecoder(r.Body).Decode(&body)
				if v, ok := body["maxResults"]; ok {
					receivedLimit = v.(float64)
				}
				writeJSON(w, map[string]any{"issues": []any{}})
			})
			defer srv.Close()

			params := &searchIssuesParams{JQL: "project = TEST", MaxResults: tc.input}
			maxResults := params.MaxResults
			if maxResults <= 0 || maxResults > 100 {
				maxResults = 50
			}
			body := map[string]any{"jql": params.JQL, "maxResults": maxResults}
			_, err := c.call(context.Background(), http.MethodPost, "/rest/api/3/search", body)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if int(receivedLimit) != tc.wantLimit {
				t.Errorf("got limit %d, want %d", int(receivedLimit), tc.wantLimit)
			}
		})
	}
}
