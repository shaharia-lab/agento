package api_test

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"

	"github.com/go-chi/chi/v5"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/api"
	"github.com/shaharia-lab/agento/internal/config"
	cfgmocks "github.com/shaharia-lab/agento/internal/config/mocks"
	"github.com/shaharia-lab/agento/internal/service"
	svcmocks "github.com/shaharia-lab/agento/internal/service/mocks"
	"github.com/shaharia-lab/agento/internal/storage"
)

// testHarness bundles the mocks and router used by every test.
type testHarness struct {
	agentSvc       *svcmocks.MockAgentService
	chatSvc        *svcmocks.MockChatService
	integrationSvc *svcmocks.MockIntegrationService
	settingsStore  *cfgmocks.MockSettingsStore
	router         chi.Router
}

func newHarness(t *testing.T) *testHarness {
	t.Helper()

	agentSvc := new(svcmocks.MockAgentService)
	chatSvc := new(svcmocks.MockChatService)
	integrationSvc := new(svcmocks.MockIntegrationService)
	settingsStore := new(cfgmocks.MockSettingsStore)

	settingsStore.On("Load").Return(config.UserSettings{}, nil)

	mgr, err := config.NewSettingsManager(settingsStore, &config.AppConfig{})
	require.NoError(t, err)

	logger := slog.Default()
	srv := api.New(agentSvc, chatSvc, integrationSvc, mgr, logger, nil)

	r := chi.NewRouter()
	srv.Mount(r)

	return &testHarness{
		agentSvc:       agentSvc,
		chatSvc:        chatSvc,
		integrationSvc: integrationSvc,
		settingsStore:  settingsStore,
		router:         r,
	}
}

func (h *testHarness) do(req *http.Request) *httptest.ResponseRecorder {
	w := httptest.NewRecorder()
	h.router.ServeHTTP(w, req)
	return w
}

// ---------- Agents ----------

func TestListAgents(t *testing.T) {
	tests := []struct {
		name       string
		agents     []*config.AgentConfig
		err        error
		wantStatus int
	}{
		{
			name:       "success with agents",
			agents:     []*config.AgentConfig{{Name: "a1", Slug: "a1"}},
			wantStatus: http.StatusOK,
		},
		{
			name:       "success empty list",
			agents:     []*config.AgentConfig{},
			wantStatus: http.StatusOK,
		},
		{
			name:       "service error",
			err:        fmt.Errorf("db down"),
			wantStatus: http.StatusInternalServerError,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.agentSvc.On("List", mock.Anything).Return(tc.agents, tc.err)

			w := h.do(httptest.NewRequest(http.MethodGet, "/agents", nil))
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusOK {
				var result []*config.AgentConfig
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Len(t, result, len(tc.agents))
			}
		})
	}
}

func TestCreateAgent(t *testing.T) {
	tests := []struct {
		name       string
		body       string
		agent      *config.AgentConfig
		err        error
		wantStatus int
	}{
		{
			name:       "success",
			body:       `{"name":"My Agent","slug":"my-agent","model":"claude-sonnet-4-6"}`,
			agent:      &config.AgentConfig{Name: "My Agent", Slug: "my-agent", Model: "claude-sonnet-4-6"},
			wantStatus: http.StatusCreated,
		},
		{
			name:       "invalid JSON",
			body:       `{invalid`,
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "validation error",
			body:       `{"name":"","slug":"my-agent"}`,
			err:        &service.ValidationError{Field: "name", Message: "name is required"},
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "conflict error",
			body:       `{"name":"My Agent","slug":"my-agent"}`,
			err:        &service.ConflictError{Resource: "agent", ID: "my-agent"},
			wantStatus: http.StatusConflict,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			if tc.body != `{invalid` {
				h.agentSvc.On("Create", mock.Anything, mock.Anything).Return(tc.agent, tc.err)
			}

			req := httptest.NewRequest(http.MethodPost, "/agents", strings.NewReader(tc.body))
			req.Header.Set("Content-Type", "application/json")
			w := h.do(req)
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusCreated {
				var result config.AgentConfig
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Equal(t, "My Agent", result.Name)
			}
		})
	}
}

func TestGetAgent(t *testing.T) {
	tests := []struct {
		name       string
		slug       string
		agent      *config.AgentConfig
		err        error
		wantStatus int
	}{
		{
			name:       "found",
			slug:       "my-agent",
			agent:      &config.AgentConfig{Name: "My Agent", Slug: "my-agent"},
			wantStatus: http.StatusOK,
		},
		{
			name:       "not found (nil)",
			slug:       "no-exist",
			agent:      nil,
			wantStatus: http.StatusNotFound,
		},
		{
			name:       "service error",
			slug:       "err-agent",
			err:        fmt.Errorf("db error"),
			wantStatus: http.StatusInternalServerError,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.agentSvc.On("Get", mock.Anything, tc.slug).Return(tc.agent, tc.err)

			w := h.do(httptest.NewRequest(http.MethodGet, "/agents/"+tc.slug, nil))
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusOK {
				var result config.AgentConfig
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Equal(t, "My Agent", result.Name)
			}
		})
	}
}

func TestUpdateAgent(t *testing.T) {
	tests := []struct {
		name       string
		slug       string
		body       string
		agent      *config.AgentConfig
		err        error
		wantStatus int
	}{
		{
			name:       "success",
			slug:       "my-agent",
			body:       `{"name":"Updated Agent","model":"claude-sonnet-4-6"}`,
			agent:      &config.AgentConfig{Name: "Updated Agent", Slug: "my-agent"},
			wantStatus: http.StatusOK,
		},
		{
			name:       "invalid JSON",
			slug:       "my-agent",
			body:       `{bad`,
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "not found",
			slug:       "no-exist",
			body:       `{"name":"X"}`,
			err:        &service.NotFoundError{Resource: "agent", ID: "no-exist"},
			wantStatus: http.StatusNotFound,
		},
		{
			name:       "validation error",
			slug:       "my-agent",
			body:       `{"name":""}`,
			err:        &service.ValidationError{Field: "name", Message: "name is required"},
			wantStatus: http.StatusBadRequest,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			if tc.body != `{bad` {
				h.agentSvc.On("Update", mock.Anything, tc.slug, mock.Anything).Return(tc.agent, tc.err)
			}

			req := httptest.NewRequest(http.MethodPut, "/agents/"+tc.slug, strings.NewReader(tc.body))
			req.Header.Set("Content-Type", "application/json")
			w := h.do(req)
			assert.Equal(t, tc.wantStatus, w.Code)
		})
	}
}

func TestDeleteAgent(t *testing.T) {
	tests := []struct {
		name       string
		slug       string
		err        error
		wantStatus int
	}{
		{
			name:       "success",
			slug:       "my-agent",
			wantStatus: http.StatusNoContent,
		},
		{
			name:       "not found",
			slug:       "no-exist",
			err:        &service.NotFoundError{Resource: "agent", ID: "no-exist"},
			wantStatus: http.StatusNotFound,
		},
		{
			name:       "service error",
			slug:       "err-agent",
			err:        fmt.Errorf("db error"),
			wantStatus: http.StatusInternalServerError,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.agentSvc.On("Delete", mock.Anything, tc.slug).Return(tc.err)

			w := h.do(httptest.NewRequest(http.MethodDelete, "/agents/"+tc.slug, nil))
			assert.Equal(t, tc.wantStatus, w.Code)
		})
	}
}

// ---------- Chats ----------

func TestListChats(t *testing.T) {
	tests := []struct {
		name       string
		sessions   []*storage.ChatSession
		err        error
		wantStatus int
	}{
		{
			name:       "success",
			sessions:   []*storage.ChatSession{{ID: "s1", Title: "Chat 1"}},
			wantStatus: http.StatusOK,
		},
		{
			name:       "service error",
			err:        fmt.Errorf("db error"),
			wantStatus: http.StatusInternalServerError,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.chatSvc.On("ListSessions", mock.Anything).Return(tc.sessions, tc.err)

			w := h.do(httptest.NewRequest(http.MethodGet, "/chats", nil))
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusOK {
				var result []*storage.ChatSession
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Len(t, result, len(tc.sessions))
			}
		})
	}
}

func TestCreateChat(t *testing.T) {
	tests := []struct {
		name       string
		body       string
		session    *storage.ChatSession
		err        error
		wantStatus int
	}{
		{
			name:       "success",
			body:       `{"agent_slug":"my-agent","working_directory":"/tmp","model":"claude-sonnet-4-6","settings_profile_id":""}`,
			session:    &storage.ChatSession{ID: "s1", Title: "New Chat", AgentSlug: "my-agent"},
			wantStatus: http.StatusCreated,
		},
		{
			name:       "invalid JSON",
			body:       `{bad`,
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "agent not found",
			body:       `{"agent_slug":"no-exist","working_directory":"/tmp","model":"","settings_profile_id":""}`,
			err:        &service.NotFoundError{Resource: "agent", ID: "no-exist"},
			wantStatus: http.StatusNotFound,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			if tc.body != `{bad` {
				h.chatSvc.On("CreateSession", mock.Anything, mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(tc.session, tc.err)
			}

			req := httptest.NewRequest(http.MethodPost, "/chats", strings.NewReader(tc.body))
			req.Header.Set("Content-Type", "application/json")
			w := h.do(req)
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusCreated {
				var result storage.ChatSession
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Equal(t, "s1", result.ID)
			}
		})
	}
}

func TestGetChat(t *testing.T) {
	tests := []struct {
		name       string
		id         string
		session    *storage.ChatSession
		messages   []storage.ChatMessage
		err        error
		wantStatus int
	}{
		{
			name:       "found",
			id:         "s1",
			session:    &storage.ChatSession{ID: "s1", Title: "Chat 1"},
			messages:   []storage.ChatMessage{{Role: "user", Content: "hello"}},
			wantStatus: http.StatusOK,
		},
		{
			name:       "not found (nil session)",
			id:         "no-exist",
			session:    nil,
			messages:   nil,
			wantStatus: http.StatusNotFound,
		},
		{
			name:       "service error",
			id:         "err-id",
			err:        fmt.Errorf("db error"),
			wantStatus: http.StatusInternalServerError,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.chatSvc.On("GetSessionWithMessages", mock.Anything, tc.id).Return(tc.session, tc.messages, tc.err)

			w := h.do(httptest.NewRequest(http.MethodGet, "/chats/"+tc.id, nil))
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusOK {
				var result map[string]json.RawMessage
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Contains(t, result, "session")
				assert.Contains(t, result, "messages")
			}
		})
	}
}

func TestDeleteChat(t *testing.T) {
	tests := []struct {
		name       string
		id         string
		err        error
		wantStatus int
	}{
		{
			name:       "success",
			id:         "s1",
			wantStatus: http.StatusNoContent,
		},
		{
			name:       "not found",
			id:         "no-exist",
			err:        &service.NotFoundError{Resource: "chat", ID: "no-exist"},
			wantStatus: http.StatusNotFound,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.chatSvc.On("DeleteSession", mock.Anything, tc.id).Return(tc.err)

			w := h.do(httptest.NewRequest(http.MethodDelete, "/chats/"+tc.id, nil))
			assert.Equal(t, tc.wantStatus, w.Code)
		})
	}
}

// ---------- Settings ----------

func TestGetSettings(t *testing.T) {
	h := newHarness(t)
	w := h.do(httptest.NewRequest(http.MethodGet, "/settings", nil))
	assert.Equal(t, http.StatusOK, w.Code)

	var result map[string]json.RawMessage
	require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
	assert.Contains(t, result, "settings")
	assert.Contains(t, result, "locked")
	assert.Contains(t, result, "model_from_env")
}

func TestUpdateSettings(t *testing.T) {
	tests := []struct {
		name       string
		body       string
		saveErr    error
		wantStatus int
	}{
		{
			name:       "success",
			body:       `{"default_working_dir":"/home/user","default_model":"claude-sonnet-4-6","onboarding_complete":true}`,
			wantStatus: http.StatusOK,
		},
		{
			name:       "invalid JSON",
			body:       `{bad`,
			wantStatus: http.StatusBadRequest,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			if tc.body != `{bad` {
				h.settingsStore.On("Save", mock.Anything).Return(tc.saveErr)
			}

			req := httptest.NewRequest(http.MethodPut, "/settings", strings.NewReader(tc.body))
			req.Header.Set("Content-Type", "application/json")
			w := h.do(req)
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusOK {
				var result map[string]json.RawMessage
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Contains(t, result, "settings")
			}
		})
	}
}

// ---------- Integrations ----------

func TestListIntegrations(t *testing.T) {
	tests := []struct {
		name         string
		integrations []*config.IntegrationConfig
		err          error
		wantStatus   int
	}{
		{
			name: "success with integrations",
			integrations: []*config.IntegrationConfig{
				{ID: "int-1", Name: "Google", Type: "google", Enabled: true},
			},
			wantStatus: http.StatusOK,
		},
		{
			name:         "success empty",
			integrations: []*config.IntegrationConfig{},
			wantStatus:   http.StatusOK,
		},
		{
			name:       "service error",
			err:        fmt.Errorf("db error"),
			wantStatus: http.StatusInternalServerError,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.integrationSvc.On("List", mock.Anything).Return(tc.integrations, tc.err)

			w := h.do(httptest.NewRequest(http.MethodGet, "/integrations", nil))
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusOK {
				var result []map[string]any
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Len(t, result, len(tc.integrations))

				// Verify scrubbing: credentials should not be present
				if len(result) > 0 {
					_, hasCredentials := result[0]["credentials"]
					assert.False(t, hasCredentials, "credentials should be scrubbed")
					_, hasAuth := result[0]["auth"]
					assert.False(t, hasAuth, "auth should be scrubbed")
				}
			}
		})
	}
}

func TestCreateIntegration(t *testing.T) {
	tests := []struct {
		name        string
		body        string
		integration *config.IntegrationConfig
		err         error
		wantStatus  int
	}{
		{
			name: "success",
			body: `{"name":"My Google","type":"google","enabled":true,"credentials":{"client_id":"cid","client_secret":"csec"},"services":{}}`,
			integration: &config.IntegrationConfig{
				ID: "int-1", Name: "My Google", Type: "google", Enabled: true,
			},
			wantStatus: http.StatusCreated,
		},
		{
			name:       "invalid JSON",
			body:       `{bad`,
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "validation error",
			body:       `{"name":"","type":"google","credentials":{"client_id":"cid","client_secret":"csec"}}`,
			err:        &service.ValidationError{Field: "name", Message: "name is required"},
			wantStatus: http.StatusBadRequest,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			if tc.body != `{bad` {
				h.integrationSvc.On("Create", mock.Anything, mock.Anything).Return(tc.integration, tc.err)
			}

			req := httptest.NewRequest(http.MethodPost, "/integrations", strings.NewReader(tc.body))
			req.Header.Set("Content-Type", "application/json")
			w := h.do(req)
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusCreated {
				var result map[string]any
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Equal(t, "int-1", result["id"])
			}
		})
	}
}

func TestGetIntegration(t *testing.T) {
	tests := []struct {
		name        string
		id          string
		integration *config.IntegrationConfig
		err         error
		wantStatus  int
	}{
		{
			name:        "found",
			id:          "int-1",
			integration: &config.IntegrationConfig{ID: "int-1", Name: "Google", Type: "google"},
			wantStatus:  http.StatusOK,
		},
		{
			name:       "not found",
			id:         "no-exist",
			err:        &service.NotFoundError{Resource: "integration", ID: "no-exist"},
			wantStatus: http.StatusNotFound,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.integrationSvc.On("Get", mock.Anything, tc.id).Return(tc.integration, tc.err)

			w := h.do(httptest.NewRequest(http.MethodGet, "/integrations/"+tc.id, nil))
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusOK {
				var result map[string]any
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Equal(t, "int-1", result["id"])
			}
		})
	}
}

func TestUpdateIntegration(t *testing.T) {
	tests := []struct {
		name        string
		id          string
		body        string
		integration *config.IntegrationConfig
		err         error
		wantStatus  int
	}{
		{
			name: "success",
			id:   "int-1",
			body: `{"name":"Updated Google","type":"google","enabled":true,"credentials":{"client_id":"cid","client_secret":"csec"},"services":{}}`,
			integration: &config.IntegrationConfig{
				ID: "int-1", Name: "Updated Google", Type: "google", Enabled: true,
			},
			wantStatus: http.StatusOK,
		},
		{
			name:       "invalid JSON",
			id:         "int-1",
			body:       `{bad`,
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "not found",
			id:         "no-exist",
			body:       `{"name":"X","type":"google","credentials":{"client_id":"c","client_secret":"s"}}`,
			err:        &service.NotFoundError{Resource: "integration", ID: "no-exist"},
			wantStatus: http.StatusNotFound,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			if tc.body != `{bad` {
				h.integrationSvc.On("Update", mock.Anything, tc.id, mock.Anything).Return(tc.integration, tc.err)
			}

			req := httptest.NewRequest(http.MethodPut, "/integrations/"+tc.id, strings.NewReader(tc.body))
			req.Header.Set("Content-Type", "application/json")
			w := h.do(req)
			assert.Equal(t, tc.wantStatus, w.Code)
		})
	}
}

func TestDeleteIntegration(t *testing.T) {
	tests := []struct {
		name       string
		id         string
		err        error
		wantStatus int
	}{
		{
			name:       "success",
			id:         "int-1",
			wantStatus: http.StatusNoContent,
		},
		{
			name:       "not found",
			id:         "no-exist",
			err:        &service.NotFoundError{Resource: "integration", ID: "no-exist"},
			wantStatus: http.StatusNotFound,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.integrationSvc.On("Delete", mock.Anything, tc.id).Return(tc.err)

			w := h.do(httptest.NewRequest(http.MethodDelete, "/integrations/"+tc.id, nil))
			assert.Equal(t, tc.wantStatus, w.Code)
		})
	}
}

func TestAvailableTools(t *testing.T) {
	tests := []struct {
		name       string
		tools      []service.AvailableTool
		err        error
		wantStatus int
	}{
		{
			name: "success",
			tools: []service.AvailableTool{
				{IntegrationID: "int-1", ToolName: "send_email", QualifiedName: "mcp__int-1__send_email", Service: "gmail"},
			},
			wantStatus: http.StatusOK,
		},
		{
			name:       "service error",
			err:        fmt.Errorf("failed"),
			wantStatus: http.StatusInternalServerError,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.integrationSvc.On("AvailableTools", mock.Anything).Return(tc.tools, tc.err)

			w := h.do(httptest.NewRequest(http.MethodGet, "/integrations/available-tools", nil))
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusOK {
				var result []service.AvailableTool
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Len(t, result, len(tc.tools))
			}
		})
	}
}

func TestStartOAuth(t *testing.T) {
	tests := []struct {
		name       string
		id         string
		authURL    string
		err        error
		wantStatus int
	}{
		{
			name:       "success",
			id:         "int-1",
			authURL:    "https://accounts.google.com/o/oauth2/auth?client_id=cid",
			wantStatus: http.StatusOK,
		},
		{
			name:       "not found",
			id:         "no-exist",
			err:        &service.NotFoundError{Resource: "integration", ID: "no-exist"},
			wantStatus: http.StatusNotFound,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.integrationSvc.On("StartOAuth", mock.Anything, tc.id).Return(tc.authURL, tc.err)

			req := httptest.NewRequest(http.MethodPost, "/integrations/"+tc.id+"/auth/start", nil)
			w := h.do(req)
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusOK {
				var result map[string]string
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Equal(t, tc.authURL, result["auth_url"])
			}
		})
	}
}

func TestGetAuthStatus(t *testing.T) {
	tests := []struct {
		name          string
		id            string
		authenticated bool
		err           error
		wantStatus    int
	}{
		{
			name:          "authenticated",
			id:            "int-1",
			authenticated: true,
			wantStatus:    http.StatusOK,
		},
		{
			name:          "not authenticated",
			id:            "int-2",
			authenticated: false,
			wantStatus:    http.StatusOK,
		},
		{
			name:       "not found",
			id:         "no-exist",
			err:        &service.NotFoundError{Resource: "integration", ID: "no-exist"},
			wantStatus: http.StatusNotFound,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			h.integrationSvc.On("GetAuthStatus", mock.Anything, tc.id).Return(tc.authenticated, tc.err)

			w := h.do(httptest.NewRequest(http.MethodGet, "/integrations/"+tc.id+"/auth/status", nil))
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusOK {
				var result map[string]bool
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Equal(t, tc.authenticated, result["authenticated"])
			}
		})
	}
}

// ---------- Version ----------

func TestVersion(t *testing.T) {
	h := newHarness(t)
	w := h.do(httptest.NewRequest(http.MethodGet, "/version", nil))
	assert.Equal(t, http.StatusOK, w.Code)

	var result map[string]string
	require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
	assert.Contains(t, result, "version")
	assert.Contains(t, result, "commit")
	assert.Contains(t, result, "build_date")
}

// ---------- Response content-type verification ----------

func TestResponseContentType(t *testing.T) {
	h := newHarness(t)
	h.agentSvc.On("List", mock.Anything).Return([]*config.AgentConfig{}, nil)

	w := h.do(httptest.NewRequest(http.MethodGet, "/agents", nil))
	assert.Equal(t, "application/json", w.Header().Get("Content-Type"))
}

// ---------- Error response format verification ----------

func TestErrorResponseFormat(t *testing.T) {
	h := newHarness(t)
	h.agentSvc.On("Get", mock.Anything, "no-exist").Return(nil, nil)

	w := h.do(httptest.NewRequest(http.MethodGet, "/agents/no-exist", nil))
	assert.Equal(t, http.StatusNotFound, w.Code)

	var result map[string]string
	require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
	assert.Contains(t, result, "error")
	assert.Equal(t, "agent not found", result["error"])
}

// ---------- Filesystem ----------

func TestFSList(t *testing.T) {
	tests := []struct {
		name       string
		path       string
		wantStatus int
	}{
		{
			name:       "list temp dir",
			path:       "/tmp",
			wantStatus: http.StatusOK,
		},
		{
			name:       "home shortcut",
			path:       "",
			wantStatus: http.StatusOK,
		},
		{
			name:       "not found",
			path:       "/nonexistent-path-xyz-abc-123",
			wantStatus: http.StatusNotFound,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			url := "/fs"
			if tc.path != "" {
				url += "?path=" + tc.path
			}
			w := h.do(httptest.NewRequest(http.MethodGet, url, nil))
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusOK {
				var result map[string]any
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &result))
				assert.Contains(t, result, "path")
				assert.Contains(t, result, "parent")
				assert.Contains(t, result, "entries")
			}
		})
	}
}

func TestFSMkdir(t *testing.T) {
	tests := []struct {
		name       string
		body       string
		wantStatus int
	}{
		{
			name:       "success",
			body:       fmt.Sprintf(`{"path":"%s"}`, filepath.Join(t.TempDir(), "newdir")),
			wantStatus: http.StatusOK,
		},
		{
			name:       "invalid JSON",
			body:       `{bad`,
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "empty path",
			body:       `{"path":""}`,
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "relative path",
			body:       `{"path":"relative/path"}`,
			wantStatus: http.StatusBadRequest,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			req := httptest.NewRequest(http.MethodPost, "/fs/mkdir", strings.NewReader(tc.body))
			req.Header.Set("Content-Type", "application/json")
			w := h.do(req)
			assert.Equal(t, tc.wantStatus, w.Code)
		})
	}
}

// ---------- Chat input/permission endpoints ----------

func TestProvideInput(t *testing.T) {
	tests := []struct {
		name       string
		body       string
		wantStatus int
	}{
		{
			name:       "no active session",
			body:       `{"answer":"yes"}`,
			wantStatus: http.StatusConflict,
		},
		{
			name:       "invalid JSON",
			body:       `{bad`,
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "empty answer",
			body:       `{"answer":""}`,
			wantStatus: http.StatusBadRequest,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			req := httptest.NewRequest(http.MethodPost, "/chats/test-id/input", strings.NewReader(tc.body))
			req.Header.Set("Content-Type", "application/json")
			w := h.do(req)
			assert.Equal(t, tc.wantStatus, w.Code)
		})
	}
}

func TestPermissionResponse(t *testing.T) {
	tests := []struct {
		name       string
		body       string
		wantStatus int
	}{
		{
			name:       "no active session",
			body:       `{"allow":true}`,
			wantStatus: http.StatusConflict,
		},
		{
			name:       "invalid JSON",
			body:       `{bad`,
			wantStatus: http.StatusBadRequest,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			req := httptest.NewRequest(http.MethodPost, "/chats/test-id/permission", strings.NewReader(tc.body))
			req.Header.Set("Content-Type", "application/json")
			w := h.do(req)
			assert.Equal(t, tc.wantStatus, w.Code)
		})
	}
}

// ---------- Send Message error paths ----------

func TestSendMessage_InvalidJSON(t *testing.T) {
	h := newHarness(t)
	req := httptest.NewRequest(http.MethodPost, "/chats/test-id/messages", strings.NewReader(`{bad`))
	req.Header.Set("Content-Type", "application/json")
	w := h.do(req)
	assert.Equal(t, http.StatusBadRequest, w.Code)
}

func TestSendMessage_EmptyContent(t *testing.T) {
	h := newHarness(t)
	req := httptest.NewRequest(http.MethodPost, "/chats/test-id/messages", strings.NewReader(`{"content":""}`))
	req.Header.Set("Content-Type", "application/json")
	w := h.do(req)
	assert.Equal(t, http.StatusBadRequest, w.Code)
}

func TestSendMessage_SessionNotFound(t *testing.T) {
	h := newHarness(t)
	h.chatSvc.On("BeginMessage", mock.Anything, "test-id", "hello", mock.Anything).
		Return(nil, nil, &service.NotFoundError{Resource: "chat", ID: "test-id"})

	req := httptest.NewRequest(http.MethodPost, "/chats/test-id/messages", strings.NewReader(`{"content":"hello"}`))
	req.Header.Set("Content-Type", "application/json")
	w := h.do(req)
	assert.Equal(t, http.StatusNotFound, w.Code)
}

func TestSendMessage_BeginMessageError(t *testing.T) {
	h := newHarness(t)
	h.chatSvc.On("BeginMessage", mock.Anything, "test-id", "hello", mock.Anything).
		Return(nil, nil, fmt.Errorf("sdk error"))

	req := httptest.NewRequest(http.MethodPost, "/chats/test-id/messages", strings.NewReader(`{"content":"hello"}`))
	req.Header.Set("Content-Type", "application/json")
	w := h.do(req)
	assert.Equal(t, http.StatusInternalServerError, w.Code)
}
