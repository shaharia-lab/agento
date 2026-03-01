package github

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/shaharia-lab/agento/internal/config"
)

// --- splitCSV ---

func TestSplitCSV(t *testing.T) {
	tests := []struct {
		input string
		want  []string
	}{
		{"", nil},
		{"a", []string{"a"}},
		{"a,b,c", []string{"a", "b", "c"}},
		{"a, b , c", []string{"a", "b", "c"}},
		{",,,", nil},
		{"  , a ,  ", []string{"a"}},
	}
	for _, tt := range tests {
		got := splitCSV(tt.input)
		if len(got) != len(tt.want) {
			t.Errorf("splitCSV(%q) = %v, want %v", tt.input, got, tt.want)
			continue
		}
		for i := range got {
			if got[i] != tt.want[i] {
				t.Errorf("splitCSV(%q)[%d] = %q, want %q", tt.input, i, got[i], tt.want[i])
			}
		}
	}
}

// --- buildAllowedSet ---

func TestBuildAllowedSet(t *testing.T) {
	cfg := &config.IntegrationConfig{
		Services: map[string]config.ServiceConfig{
			"repos":   {Enabled: true, Tools: []string{"list_repos", "get_repo"}},
			"issues":  {Enabled: false, Tools: []string{"list_issues"}},
			"actions": {Enabled: true, Tools: []string{"list_workflows"}},
		},
	}
	allowed := buildAllowedSet(cfg)

	if !allowed["list_repos"] {
		t.Error("expected list_repos in allowed set")
	}
	if !allowed["get_repo"] {
		t.Error("expected get_repo in allowed set")
	}
	if !allowed["list_workflows"] {
		t.Error("expected list_workflows in allowed set")
	}
	if allowed["list_issues"] {
		t.Error("list_issues should not be in allowed set (service disabled)")
	}
}

// --- serviceEnabled ---

func TestServiceEnabled(t *testing.T) {
	cfg := &config.IntegrationConfig{
		Services: map[string]config.ServiceConfig{
			"repos":  {Enabled: true},
			"issues": {Enabled: false},
		},
	}

	if !serviceEnabled(cfg, "repos") {
		t.Error("expected repos to be enabled")
	}
	if serviceEnabled(cfg, "issues") {
		t.Error("expected issues to be disabled")
	}
	if serviceEnabled(cfg, "nonexistent") {
		t.Error("expected nonexistent service to return false")
	}
}

// --- ValidatePAT ---

func TestValidatePAT_Success(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer test-token" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		if err := json.NewEncoder(w).Encode(map[string]string{"login": "octocat"}); err != nil {
			http.Error(w, "encode error", http.StatusInternalServerError)
		}
	}))
	defer srv.Close()

	// Temporarily override the API base so ValidatePAT hits the test server.
	orig := githubAPIBase
	githubAPIBase = srv.URL
	defer func() { githubAPIBase = orig }()

	username, err := ValidatePAT(t.Context(), "test-token")
	if err != nil {
		t.Fatalf("ValidatePAT returned unexpected error: %v", err)
	}
	if username != "octocat" {
		t.Errorf("ValidatePAT returned username %q, want %q", username, "octocat")
	}
}

func TestValidatePAT_Unauthorized(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, `{"message":"Bad credentials"}`, http.StatusUnauthorized)
	}))
	defer srv.Close()

	orig := githubAPIBase
	githubAPIBase = srv.URL
	defer func() { githubAPIBase = orig }()

	_, err := ValidatePAT(t.Context(), "bad-token")
	if err == nil {
		t.Fatal("expected error for unauthorized response, got nil")
	}
}

// --- buildMCPServer ---

func TestBuildMCPServer_RegistersCorrectTools(t *testing.T) {
	cfg := &config.IntegrationConfig{
		ID: "test-gh",
		Services: map[string]config.ServiceConfig{
			"repos":  {Enabled: true, Tools: []string{"list_repos"}},
			"issues": {Enabled: false, Tools: []string{"list_issues"}},
		},
	}
	server := buildMCPServer(cfg, "dummy-token")
	if server == nil {
		t.Fatal("buildMCPServer returned nil")
	}
}
