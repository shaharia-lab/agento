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
	"golang.org/x/oauth2"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/integrations"
	"github.com/shaharia-lab/agento/internal/storage/mocks"
)

// helper to build a silent logger for tests.
func testLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}

// helper to build a valid IntegrationConfig with all required fields.
func validConfig() *config.IntegrationConfig {
	return &config.IntegrationConfig{
		Name: "My Google",
		Type: "google",
		Credentials: config.GoogleCredentials{
			ClientID:     "cid",
			ClientSecret: "csecret",
		},
	}
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

func TestIntegrationService_List(t *testing.T) {
	tests := []struct {
		name      string
		setup     func(m *mocks.MockIntegrationStore)
		wantLen   int
		wantErr   bool
		errSubstr string
	}{
		{
			name: "returns_all_integrations",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("List").Return([]*config.IntegrationConfig{
					{ID: "a", Name: "A"},
					{ID: "b", Name: "B"},
				}, nil)
			},
			wantLen: 2,
		},
		{
			name: "returns_empty_list",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("List").Return([]*config.IntegrationConfig{}, nil)
			},
			wantLen: 0,
		},
		{
			name: "propagates_store_error",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("List").Return(nil, errors.New("db failure"))
			},
			wantErr:   true,
			errSubstr: "db failure",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			store := new(mocks.MockIntegrationStore)
			tc.setup(store)
			svc := NewIntegrationService(store, nil, testLogger(), context.Background())

			got, err := svc.List(context.Background())
			if tc.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tc.errSubstr)
			} else {
				assert.NoError(t, err)
				assert.Len(t, got, tc.wantLen)
			}
			store.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// Get
// ---------------------------------------------------------------------------

func TestIntegrationService_Get(t *testing.T) {
	tests := []struct {
		name      string
		id        string
		setup     func(m *mocks.MockIntegrationStore)
		wantID    string
		wantErr   bool
		errType   interface{}
		errSubstr string
	}{
		{
			name: "returns_existing_integration",
			id:   "int-1",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "int-1").Return(&config.IntegrationConfig{ID: "int-1", Name: "A"}, nil)
			},
			wantID: "int-1",
		},
		{
			name: "returns_not_found_when_store_returns_nil",
			id:   "missing",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "missing").Return(nil, nil)
			},
			wantErr: true,
			errType: &NotFoundError{},
		},
		{
			name: "propagates_store_error",
			id:   "err-id",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "err-id").Return(nil, errors.New("disk error"))
			},
			wantErr:   true,
			errSubstr: "disk error",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			store := new(mocks.MockIntegrationStore)
			tc.setup(store)
			svc := NewIntegrationService(store, nil, testLogger(), context.Background())

			got, err := svc.Get(context.Background(), tc.id)
			if tc.wantErr {
				assert.Error(t, err)
				if tc.errType != nil {
					assert.IsType(t, tc.errType, err)
				}
				if tc.errSubstr != "" {
					assert.Contains(t, err.Error(), tc.errSubstr)
				}
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tc.wantID, got.ID)
			}
			store.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

func TestIntegrationService_Create(t *testing.T) {
	tests := []struct {
		name      string
		input     *config.IntegrationConfig
		setup     func(m *mocks.MockIntegrationStore)
		wantErr   bool
		errType   interface{}
		errField  string // for ValidationError
		checkFunc func(t *testing.T, got *config.IntegrationConfig)
	}{
		{
			name:  "success_generates_id_and_timestamps",
			input: validConfig(),
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Save", mock.AnythingOfType("*config.IntegrationConfig")).Return(nil)
			},
			checkFunc: func(t *testing.T, got *config.IntegrationConfig) {
				assert.NotEmpty(t, got.ID, "should generate UUID")
				assert.False(t, got.CreatedAt.IsZero(), "should set CreatedAt")
				assert.False(t, got.UpdatedAt.IsZero(), "should set UpdatedAt")
				assert.Equal(t, got.CreatedAt, got.UpdatedAt, "CreatedAt and UpdatedAt should be equal on create")
				assert.NotNil(t, got.Services, "should initialize Services map")
			},
		},
		{
			name: "success_preserves_existing_id",
			input: func() *config.IntegrationConfig {
				c := validConfig()
				c.ID = "custom-id"
				return c
			}(),
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Save", mock.MatchedBy(func(c *config.IntegrationConfig) bool {
					return c.ID == "custom-id"
				})).Return(nil)
			},
			checkFunc: func(t *testing.T, got *config.IntegrationConfig) {
				assert.Equal(t, "custom-id", got.ID)
			},
		},
		{
			name: "success_preserves_existing_services",
			input: func() *config.IntegrationConfig {
				c := validConfig()
				c.Services = map[string]config.ServiceConfig{
					"calendar": {Enabled: true, Tools: []string{"list_events"}},
				}
				return c
			}(),
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Save", mock.AnythingOfType("*config.IntegrationConfig")).Return(nil)
			},
			checkFunc: func(t *testing.T, got *config.IntegrationConfig) {
				assert.Contains(t, got.Services, "calendar")
				assert.True(t, got.Services["calendar"].Enabled)
			},
		},
		{
			name:     "validation_error_missing_name",
			input:    &config.IntegrationConfig{Type: "google", Credentials: config.GoogleCredentials{ClientID: "a", ClientSecret: "b"}},
			wantErr:  true,
			errType:  &ValidationError{},
			errField: "name",
		},
		{
			name:     "validation_error_missing_type",
			input:    &config.IntegrationConfig{Name: "A", Credentials: config.GoogleCredentials{ClientID: "a", ClientSecret: "b"}},
			wantErr:  true,
			errType:  &ValidationError{},
			errField: "type",
		},
		{
			name:     "validation_error_missing_client_id",
			input:    &config.IntegrationConfig{Name: "A", Type: "google", Credentials: config.GoogleCredentials{ClientSecret: "b"}},
			wantErr:  true,
			errType:  &ValidationError{},
			errField: "credentials.client_id",
		},
		{
			name:     "validation_error_missing_client_secret",
			input:    &config.IntegrationConfig{Name: "A", Type: "google", Credentials: config.GoogleCredentials{ClientID: "a"}},
			wantErr:  true,
			errType:  &ValidationError{},
			errField: "credentials.client_secret",
		},
		{
			name:  "store_save_error",
			input: validConfig(),
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Save", mock.AnythingOfType("*config.IntegrationConfig")).Return(errors.New("write failed"))
			},
			wantErr: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			store := new(mocks.MockIntegrationStore)
			if tc.setup != nil {
				tc.setup(store)
			}
			svc := NewIntegrationService(store, nil, testLogger(), context.Background())

			got, err := svc.Create(context.Background(), tc.input)
			if tc.wantErr {
				assert.Error(t, err)
				if tc.errType != nil {
					assert.IsType(t, tc.errType, err)
				}
				if tc.errField != "" {
					var ve *ValidationError
					if errors.As(err, &ve) {
						assert.Equal(t, tc.errField, ve.Field)
					}
				}
				assert.Nil(t, got)
			} else {
				assert.NoError(t, err)
				assert.NotNil(t, got)
				if tc.checkFunc != nil {
					tc.checkFunc(t, got)
				}
			}
			store.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

func TestIntegrationService_Update(t *testing.T) {
	existingToken := &oauth2.Token{AccessToken: "old-token"}
	now := time.Now().UTC()

	tests := []struct {
		name      string
		id        string
		input     *config.IntegrationConfig
		setup     func(m *mocks.MockIntegrationStore)
		wantErr   bool
		errType   interface{}
		checkFunc func(t *testing.T, got *config.IntegrationConfig)
	}{
		{
			name:  "success_preserves_auth_when_nil",
			id:    "int-1",
			input: &config.IntegrationConfig{Name: "Updated", Auth: nil},
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "int-1").Return(&config.IntegrationConfig{
					ID:        "int-1",
					Name:      "Old",
					Auth:      existingToken,
					CreatedAt: now,
				}, nil)
				m.On("Save", mock.AnythingOfType("*config.IntegrationConfig")).Return(nil)
			},
			checkFunc: func(t *testing.T, got *config.IntegrationConfig) {
				assert.Equal(t, "int-1", got.ID, "should preserve ID")
				assert.Equal(t, existingToken, got.Auth, "should preserve existing auth token")
				assert.Equal(t, now, got.CreatedAt, "should preserve CreatedAt")
				assert.True(t, got.UpdatedAt.After(now.Add(-time.Second)), "should update UpdatedAt")
			},
		},
		{
			name: "success_replaces_auth_when_provided",
			id:   "int-1",
			input: &config.IntegrationConfig{
				Name: "Updated",
				Auth: &oauth2.Token{AccessToken: "new-token"},
			},
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "int-1").Return(&config.IntegrationConfig{
					ID:        "int-1",
					Name:      "Old",
					Auth:      existingToken,
					CreatedAt: now,
				}, nil)
				m.On("Save", mock.AnythingOfType("*config.IntegrationConfig")).Return(nil)
			},
			checkFunc: func(t *testing.T, got *config.IntegrationConfig) {
				assert.Equal(t, "new-token", got.Auth.AccessToken)
			},
		},
		{
			name:  "not_found_error",
			id:    "no-exist",
			input: &config.IntegrationConfig{Name: "X"},
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "no-exist").Return(nil, nil)
			},
			wantErr: true,
			errType: &NotFoundError{},
		},
		{
			name:  "store_get_error",
			id:    "err-id",
			input: &config.IntegrationConfig{Name: "X"},
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "err-id").Return(nil, errors.New("db read error"))
			},
			wantErr: true,
		},
		{
			name:  "store_save_error",
			id:    "int-1",
			input: &config.IntegrationConfig{Name: "Updated"},
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "int-1").Return(&config.IntegrationConfig{
					ID:        "int-1",
					CreatedAt: now,
				}, nil)
				m.On("Save", mock.AnythingOfType("*config.IntegrationConfig")).Return(errors.New("write failed"))
			},
			wantErr: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			store := new(mocks.MockIntegrationStore)
			tc.setup(store)

			// Use a real registry so that Reload is callable (it just reads from store).
			registry := integrations.NewRegistry(store, testLogger())
			svc := NewIntegrationService(store, registry, testLogger(), context.Background())

			got, err := svc.Update(context.Background(), tc.id, tc.input)
			if tc.wantErr {
				assert.Error(t, err)
				if tc.errType != nil {
					assert.IsType(t, tc.errType, err)
				}
			} else {
				assert.NoError(t, err)
				assert.NotNil(t, got)
				if tc.checkFunc != nil {
					tc.checkFunc(t, got)
				}
			}
			store.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

func TestIntegrationService_Delete(t *testing.T) {
	tests := []struct {
		name      string
		id        string
		setup     func(m *mocks.MockIntegrationStore)
		wantErr   bool
		errType   interface{}
		errSubstr string
	}{
		{
			name: "success",
			id:   "int-1",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "int-1").Return(&config.IntegrationConfig{ID: "int-1"}, nil)
				m.On("Delete", "int-1").Return(nil)
			},
		},
		{
			name: "not_found",
			id:   "no-exist",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "no-exist").Return(nil, nil)
			},
			wantErr: true,
			errType: &NotFoundError{},
		},
		{
			name: "store_get_error",
			id:   "err-id",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "err-id").Return(nil, errors.New("db error"))
			},
			wantErr:   true,
			errSubstr: "db error",
		},
		{
			name: "store_delete_error",
			id:   "int-1",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "int-1").Return(&config.IntegrationConfig{ID: "int-1"}, nil)
				m.On("Delete", "int-1").Return(errors.New("delete failed"))
			},
			wantErr:   true,
			errSubstr: "delete failed",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			store := new(mocks.MockIntegrationStore)
			tc.setup(store)

			registry := integrations.NewRegistry(store, testLogger())
			svc := NewIntegrationService(store, registry, testLogger(), context.Background())

			err := svc.Delete(context.Background(), tc.id)
			if tc.wantErr {
				assert.Error(t, err)
				if tc.errType != nil {
					assert.IsType(t, tc.errType, err)
				}
				if tc.errSubstr != "" {
					assert.Contains(t, err.Error(), tc.errSubstr)
				}
			} else {
				assert.NoError(t, err)
			}
			store.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// GetAuthStatus
// ---------------------------------------------------------------------------

func TestIntegrationService_GetAuthStatus(t *testing.T) {
	tests := []struct {
		name       string
		id         string
		oauthFlows map[string]*oauthState
		setup      func(m *mocks.MockIntegrationStore)
		wantAuth   bool
		wantErr    bool
		errType    interface{}
	}{
		{
			name: "active_flow_authenticated",
			id:   "int-1",
			oauthFlows: map[string]*oauthState{
				"int-1": {authenticated: true, done: true},
			},
			wantAuth: true,
		},
		{
			name: "active_flow_not_authenticated",
			id:   "int-1",
			oauthFlows: map[string]*oauthState{
				"int-1": {authenticated: false, done: false},
			},
			wantAuth: false,
		},
		{
			name: "active_flow_with_error",
			id:   "int-1",
			oauthFlows: map[string]*oauthState{
				"int-1": {err: errors.New("oauth failed"), done: true},
			},
			wantErr: true,
		},
		{
			name: "no_flow_has_stored_token",
			id:   "int-1",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "int-1").Return(&config.IntegrationConfig{
					ID:   "int-1",
					Auth: &oauth2.Token{AccessToken: "tok"},
				}, nil)
			},
			wantAuth: true,
		},
		{
			name: "no_flow_no_stored_token",
			id:   "int-1",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "int-1").Return(&config.IntegrationConfig{
					ID:   "int-1",
					Auth: nil,
				}, nil)
			},
			wantAuth: false,
		},
		{
			name: "no_flow_store_returns_nil",
			id:   "missing",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "missing").Return(nil, nil)
			},
			wantErr: true,
			errType: &NotFoundError{},
		},
		{
			name: "no_flow_store_error",
			id:   "err-id",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("Get", "err-id").Return(nil, errors.New("db error"))
			},
			wantErr: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			store := new(mocks.MockIntegrationStore)
			if tc.setup != nil {
				tc.setup(store)
			}

			svc := NewIntegrationService(store, nil, testLogger(), context.Background())

			// Inject oauthFlows state if provided.
			if tc.oauthFlows != nil {
				impl := svc.(*integrationService)
				impl.mu.Lock()
				for k, v := range tc.oauthFlows {
					impl.oauthFlows[k] = v
				}
				impl.mu.Unlock()
			}

			auth, err := svc.GetAuthStatus(context.Background(), tc.id)
			if tc.wantErr {
				assert.Error(t, err)
				if tc.errType != nil {
					assert.IsType(t, tc.errType, err)
				}
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tc.wantAuth, auth)
			}
			store.AssertExpectations(t)
		})
	}
}

// ---------------------------------------------------------------------------
// AvailableTools
// ---------------------------------------------------------------------------

func TestIntegrationService_AvailableTools(t *testing.T) {
	tests := []struct {
		name      string
		setup     func(m *mocks.MockIntegrationStore)
		wantLen   int
		wantErr   bool
		checkFunc func(t *testing.T, tools []AvailableTool)
	}{
		{
			name: "returns_tools_from_enabled_authenticated_integrations",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("List").Return([]*config.IntegrationConfig{
					{
						ID:      "g1",
						Name:    "Google 1",
						Enabled: true,
						Auth:    &oauth2.Token{AccessToken: "tok"},
						Services: map[string]config.ServiceConfig{
							"calendar": {Enabled: true, Tools: []string{"list_events", "create_event"}},
							"gmail":    {Enabled: true, Tools: []string{"send_email"}},
						},
					},
				}, nil)
			},
			wantLen: 3,
			checkFunc: func(t *testing.T, tools []AvailableTool) {
				names := make(map[string]bool)
				for _, tool := range tools {
					names[tool.ToolName] = true
					assert.Equal(t, "g1", tool.IntegrationID)
					assert.Equal(t, "Google 1", tool.IntegrationName)
					assert.Contains(t, tool.QualifiedName, "mcp__g1__")
				}
				assert.True(t, names["list_events"])
				assert.True(t, names["create_event"])
				assert.True(t, names["send_email"])
			},
		},
		{
			name: "skips_disabled_integrations",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("List").Return([]*config.IntegrationConfig{
					{
						ID:      "g1",
						Name:    "Disabled",
						Enabled: false,
						Auth:    &oauth2.Token{AccessToken: "tok"},
						Services: map[string]config.ServiceConfig{
							"calendar": {Enabled: true, Tools: []string{"list_events"}},
						},
					},
				}, nil)
			},
			wantLen: 0,
		},
		{
			name: "skips_unauthenticated_integrations",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("List").Return([]*config.IntegrationConfig{
					{
						ID:      "g1",
						Name:    "No Auth",
						Enabled: true,
						Auth:    nil,
						Services: map[string]config.ServiceConfig{
							"calendar": {Enabled: true, Tools: []string{"list_events"}},
						},
					},
				}, nil)
			},
			wantLen: 0,
		},
		{
			name: "skips_disabled_services",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("List").Return([]*config.IntegrationConfig{
					{
						ID:      "g1",
						Name:    "G",
						Enabled: true,
						Auth:    &oauth2.Token{AccessToken: "tok"},
						Services: map[string]config.ServiceConfig{
							"calendar": {Enabled: false, Tools: []string{"list_events"}},
							"gmail":    {Enabled: true, Tools: []string{"send_email"}},
						},
					},
				}, nil)
			},
			wantLen: 1,
			checkFunc: func(t *testing.T, tools []AvailableTool) {
				assert.Equal(t, "send_email", tools[0].ToolName)
				assert.Equal(t, "gmail", tools[0].Service)
			},
		},
		{
			name: "returns_empty_for_no_integrations",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("List").Return([]*config.IntegrationConfig{}, nil)
			},
			wantLen: 0,
		},
		{
			name: "store_error",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("List").Return(nil, errors.New("db failure"))
			},
			wantErr: true,
		},
		{
			name: "multiple_integrations_mixed",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("List").Return([]*config.IntegrationConfig{
					{
						ID: "g1", Name: "G1", Enabled: true,
						Auth: &oauth2.Token{AccessToken: "tok"},
						Services: map[string]config.ServiceConfig{
							"calendar": {Enabled: true, Tools: []string{"list_events"}},
						},
					},
					{
						ID: "g2", Name: "G2", Enabled: false,
						Auth: &oauth2.Token{AccessToken: "tok"},
						Services: map[string]config.ServiceConfig{
							"gmail": {Enabled: true, Tools: []string{"send_email"}},
						},
					},
					{
						ID: "g3", Name: "G3", Enabled: true,
						Auth: &oauth2.Token{AccessToken: "tok"},
						Services: map[string]config.ServiceConfig{
							"drive": {Enabled: true, Tools: []string{"list_files", "download_file"}},
						},
					},
				}, nil)
			},
			wantLen: 3, // 1 from g1 + 0 from g2 (disabled) + 2 from g3
			checkFunc: func(t *testing.T, tools []AvailableTool) {
				ids := make(map[string]int)
				for _, tool := range tools {
					ids[tool.IntegrationID]++
				}
				assert.Equal(t, 1, ids["g1"])
				assert.Equal(t, 0, ids["g2"])
				assert.Equal(t, 2, ids["g3"])
			},
		},
		{
			name: "qualified_name_format",
			setup: func(m *mocks.MockIntegrationStore) {
				m.On("List").Return([]*config.IntegrationConfig{
					{
						ID: "my-id", Name: "G", Enabled: true,
						Auth: &oauth2.Token{AccessToken: "tok"},
						Services: map[string]config.ServiceConfig{
							"calendar": {Enabled: true, Tools: []string{"list_events"}},
						},
					},
				}, nil)
			},
			wantLen: 1,
			checkFunc: func(t *testing.T, tools []AvailableTool) {
				assert.Equal(t, "mcp__my-id__list_events", tools[0].QualifiedName)
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			store := new(mocks.MockIntegrationStore)
			tc.setup(store)
			svc := NewIntegrationService(store, nil, testLogger(), context.Background())

			tools, err := svc.AvailableTools(context.Background())
			if tc.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
				assert.Len(t, tools, tc.wantLen)
				if tc.checkFunc != nil {
					tc.checkFunc(t, tools)
				}
			}
			store.AssertExpectations(t)
		})
	}
}
