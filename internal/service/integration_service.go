package service

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
	"golang.org/x/oauth2"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/integrations"
	"github.com/shaharia-lab/agento/internal/integrations/confluence"
	githubintegration "github.com/shaharia-lab/agento/internal/integrations/github"
	"github.com/shaharia-lab/agento/internal/integrations/google"
	"github.com/shaharia-lab/agento/internal/integrations/jira"
	slackintegration "github.com/shaharia-lab/agento/internal/integrations/slack"
	"github.com/shaharia-lab/agento/internal/integrations/telegram"
	"github.com/shaharia-lab/agento/internal/storage"
)

// AvailableTool describes a single tool exposed by an integration.
type AvailableTool struct {
	IntegrationID   string `json:"integration_id"`
	IntegrationName string `json:"integration_name"`
	ToolName        string `json:"tool_name"`      // bare name e.g. "send_email"
	QualifiedName   string `json:"qualified_name"` // "mcp__my-google__send_email"
	Service         string `json:"service"`
}

// IntegrationService defines the business logic interface for managing integrations.
type IntegrationService interface {
	List(ctx context.Context) ([]*config.IntegrationConfig, error)
	Get(ctx context.Context, id string) (*config.IntegrationConfig, error)
	Create(ctx context.Context, cfg *config.IntegrationConfig) (*config.IntegrationConfig, error)
	Update(ctx context.Context, id string, cfg *config.IntegrationConfig) (*config.IntegrationConfig, error)
	Delete(ctx context.Context, id string) error
	StartOAuth(ctx context.Context, id string) (authURL string, err error)
	GetAuthStatus(ctx context.Context, id string) (authenticated bool, err error)
	AvailableTools(ctx context.Context) ([]AvailableTool, error)
	ValidateTokenAuth(ctx context.Context, cfg *config.IntegrationConfig) error
}

// Shared field name constants used in validation errors.
const (
	fieldCredentialsBotToken   = "credentials.bot_token"
	fieldCredentialsSiteURL    = "credentials.site_url"
	errFmtSavingValidatedInteg = "saving validated integration: %w"
)

// oauthState tracks an in-progress OAuth flow.
type oauthState struct {
	authenticated bool
	err           error
	done          bool
}

// integrationService is the default implementation.
type integrationService struct {
	store    storage.IntegrationStore
	registry *integrations.IntegrationRegistry
	logger   *slog.Logger

	mu         sync.Mutex
	oauthFlows map[string]*oauthState // integration id → state
}

// NewIntegrationService returns a new IntegrationService.
func NewIntegrationService(
	store storage.IntegrationStore,
	registry *integrations.IntegrationRegistry,
	logger *slog.Logger,
) IntegrationService {
	return &integrationService{
		store:      store,
		registry:   registry,
		logger:     logger,
		oauthFlows: make(map[string]*oauthState),
	}
}

// validateIntegrationCredentials performs type-specific credential validation.
func validateIntegrationCredentials(cfg *config.IntegrationConfig) error {
	switch cfg.Type {
	case "google":
		return validateGoogleCredentials(cfg)
	case "confluence":
		return validateAtlassianCredentials(cfg)
	case "telegram":
		return validateTelegramCredentials(cfg)
	case "jira":
		return validateJiraCredentials(cfg)
	case "github":
		return validateGitHubCredentials(cfg)
	case "slack":
		return validateSlackCredentials(cfg)
	default:
		if len(cfg.Credentials) == 0 {
			return &ValidationError{Field: "credentials", Message: "credentials are required"}
		}
	}
	return nil
}

func validateGoogleCredentials(cfg *config.IntegrationConfig) error {
	var creds config.GoogleCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid google credentials: " + err.Error()}
	}
	if creds.ClientID == "" {
		return &ValidationError{Field: "credentials.client_id", Message: "client_id is required"}
	}
	if creds.ClientSecret == "" {
		return &ValidationError{Field: "credentials.client_secret", Message: "client_secret is required"}
	}
	return nil
}

func validateAtlassianCredentials(cfg *config.IntegrationConfig) error {
	var creds config.AtlassianCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid confluence credentials: " + err.Error()}
	}
	if creds.SiteURL == "" {
		return &ValidationError{Field: fieldCredentialsSiteURL, Message: "site_url is required"}
	}
	if _, err := confluence.ValidateSiteURL(creds.SiteURL); err != nil {
		return &ValidationError{Field: fieldCredentialsSiteURL, Message: err.Error()}
	}
	if creds.Email == "" {
		return &ValidationError{Field: "credentials.email", Message: "email is required"}
	}
	if creds.APIToken == "" {
		return &ValidationError{Field: "credentials.api_token", Message: "api_token is required"}
	}
	return nil
}

func validateTelegramCredentials(cfg *config.IntegrationConfig) error {
	var creds config.TelegramCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid telegram credentials: " + err.Error()}
	}
	if creds.BotToken == "" {
		return &ValidationError{Field: fieldCredentialsBotToken, Message: "bot_token is required"}
	}
	return nil
}

func validateJiraCredentials(cfg *config.IntegrationConfig) error {
	var creds config.AtlassianCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid jira credentials: " + err.Error()}
	}
	if creds.SiteURL == "" {
		return &ValidationError{Field: fieldCredentialsSiteURL, Message: "site_url is required"}
	}
	u, err := url.Parse(creds.SiteURL)
	if err != nil || (u.Scheme != "https" && u.Scheme != "http") || u.Host == "" {
		return &ValidationError{Field: fieldCredentialsSiteURL, Message: "site_url must be a valid http or https URL"}
	}
	// Normalize: strip trailing slash so URL concatenation is consistent.
	creds.SiteURL = strings.TrimRight(creds.SiteURL, "/")
	if creds.Email == "" {
		return &ValidationError{Field: "credentials.email", Message: "email is required"}
	}
	if creds.APIToken == "" {
		return &ValidationError{Field: "credentials.api_token", Message: "api_token is required"}
	}
	// Save normalized credentials back to the config so the stored value is canonical.
	return cfg.SetCredentials(creds)
}

func validateGitHubCredentials(cfg *config.IntegrationConfig) error {
	var creds config.GitHubCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid github credentials: " + err.Error()}
	}
	if creds.AuthMode != "pat" {
		return &ValidationError{Field: "credentials.auth_mode", Message: "only 'pat' auth mode is currently supported"}
	}
	if creds.PersonalAccessToken == "" {
		return &ValidationError{Field: "credentials.personal_access_token", Message: "personal_access_token is required"}
	}
	return nil
}

func validateSlackCredentials(cfg *config.IntegrationConfig) error {
	var creds config.SlackCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid slack credentials: " + err.Error()}
	}
	switch creds.AuthMode {
	case "bot_token":
		if creds.BotToken == "" {
			return &ValidationError{Field: fieldCredentialsBotToken, Message: "bot_token is required"}
		}
	case "oauth":
		if creds.ClientID == "" {
			return &ValidationError{Field: "credentials.client_id", Message: "client_id is required"}
		}
		if creds.ClientSecret == "" {
			return &ValidationError{Field: "credentials.client_secret", Message: "client_secret is required"}
		}
	default:
		return &ValidationError{Field: "credentials.auth_mode", Message: "auth_mode must be 'bot_token' or 'oauth'"}
	}
	return nil
}

func (s *integrationService) List(_ context.Context) ([]*config.IntegrationConfig, error) {
	return s.store.List()
}

func (s *integrationService) Get(_ context.Context, id string) (*config.IntegrationConfig, error) {
	cfg, err := s.store.Get(id)
	if err != nil {
		return nil, err
	}
	if cfg == nil {
		return nil, &NotFoundError{Resource: "integration", ID: id}
	}
	return cfg, nil
}

func (s *integrationService) Create(
	_ context.Context, cfg *config.IntegrationConfig,
) (*config.IntegrationConfig, error) {
	if cfg.Name == "" {
		return nil, &ValidationError{Field: "name", Message: "name is required"}
	}
	if cfg.Type == "" {
		return nil, &ValidationError{Field: "type", Message: "type is required"}
	}

	if err := validateIntegrationCredentials(cfg); err != nil {
		return nil, err
	}

	if cfg.ID == "" {
		cfg.ID = uuid.New().String()
	}
	now := time.Now().UTC()
	cfg.CreatedAt = now
	cfg.UpdatedAt = now

	if cfg.Services == nil {
		cfg.Services = make(map[string]config.ServiceConfig)
	}

	if err := s.store.Save(cfg); err != nil {
		return nil, fmt.Errorf("saving integration: %w", err)
	}
	s.logger.Info("integration created", "id", cfg.ID, "name", cfg.Name)
	return cfg, nil
}

func (s *integrationService) Update(
	ctx context.Context, id string, cfg *config.IntegrationConfig,
) (*config.IntegrationConfig, error) {
	existing, err := s.Get(ctx, id)
	if err != nil {
		return nil, err
	}

	cfg.ID = id
	cfg.CreatedAt = existing.CreatedAt
	cfg.UpdatedAt = time.Now().UTC()
	// Preserve existing auth token unless the caller provides a new one.
	if !cfg.IsAuthenticated() {
		cfg.Auth = existing.Auth
	}

	if err := s.store.Save(cfg); err != nil {
		return nil, fmt.Errorf("saving integration: %w", err)
	}

	// Reload the in-process MCP server with the new config. Use context.Background()
	// so the reload is not canceled if the HTTP client disconnects before it completes.
	if reloadErr := s.registry.Reload(context.Background(), id); reloadErr != nil {
		s.logger.Warn("failed to reload integration server after update", "id", id, "error", reloadErr)
	}

	s.logger.Info("integration updated", "id", id)
	return cfg, nil
}

func (s *integrationService) Delete(ctx context.Context, id string) error {
	if _, err := s.Get(ctx, id); err != nil {
		return err
	}
	s.registry.Stop(id)
	if err := s.store.Delete(id); err != nil {
		return fmt.Errorf("deleting integration: %w", err)
	}
	s.logger.Info("integration deleted", "id", id)
	return nil
}

func (s *integrationService) StartOAuth(ctx context.Context, id string) (string, error) {
	cfg, err := s.store.Get(id)
	if err != nil {
		return "", err
	}
	if cfg == nil {
		return "", &NotFoundError{Resource: "integration", ID: id}
	}

	if cfg.Type != "google" && cfg.Type != "slack" {
		msg := fmt.Sprintf("OAuth flow is not supported for integration type %q", cfg.Type)
		return "", &ValidationError{Field: "type", Message: msg}
	}

	port, err := integrations.FreePort()
	if err != nil {
		return "", fmt.Errorf("finding free port: %w", err)
	}

	state := &oauthState{}
	s.mu.Lock()
	s.oauthFlows[id] = state
	s.mu.Unlock()

	var authURL string
	var buildErr error

	// Detach from the request context so the callback server outlives the HTTP request,
	// then apply a 10-minute deadline for the OAuth flow.
	callbackCtx, cancelCallback := context.WithTimeout(context.WithoutCancel(ctx), 10*time.Minute)
	// cancelCallback is idempotent — safe to call multiple times (e.g. also called by onToken).
	defer cancelCallback()

	onToken := func(tok *oauth2.Token, tokErr error) {
		defer cancelCallback()
		s.handleOAuthToken(id, state, tok, tokErr)
	}

	switch cfg.Type {
	case "google":
		authURL, buildErr = google.BuildAuthURL(cfg, port)
		if buildErr != nil {
			return "", fmt.Errorf("building auth URL: %w", buildErr)
		}
		if err := google.StartCallbackServer(callbackCtx, port, cfg, onToken, s.logger); err != nil {
			return "", fmt.Errorf("starting callback server: %w", err)
		}
	case "slack":
		authURL, buildErr = slackintegration.BuildAuthURL(cfg, port)
		if buildErr != nil {
			return "", fmt.Errorf("building auth URL: %w", buildErr)
		}
		if err := slackintegration.StartCallbackServer(callbackCtx, port, cfg, onToken, s.logger); err != nil {
			return "", fmt.Errorf("starting callback server: %w", err)
		}
	}

	return authURL, nil
}

func (s *integrationService) handleOAuthToken(id string, state *oauthState, tok *oauth2.Token, tokErr error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	state.done = true
	if tokErr != nil {
		state.err = tokErr
		s.logger.Warn("OAuth flow failed", "id", id, "error", tokErr)
		return
	}

	// Save the token to the integration config.
	latestCfg, loadErr := s.store.Get(id)
	if loadErr != nil || latestCfg == nil {
		state.err = fmt.Errorf("loading integration after OAuth: %w", loadErr)
		return
	}
	if setErr := latestCfg.SetOAuthToken(tok); setErr != nil {
		state.err = fmt.Errorf("setting oauth token: %w", setErr)
		return
	}
	latestCfg.UpdatedAt = time.Now().UTC()
	if saveErr := s.store.Save(latestCfg); saveErr != nil {
		state.err = fmt.Errorf("saving token: %w", saveErr)
		return
	}

	state.authenticated = true
	s.logger.Info("OAuth completed, starting integration server", "id", id)

	// Start the MCP server for this newly-authenticated integration.
	go func() {
		if startErr := s.registry.Reload(context.Background(), id); startErr != nil {
			s.logger.Warn("failed to start integration server after OAuth", "id", id, "error", startErr)
		}
	}()
}

func (s *integrationService) GetAuthStatus(_ context.Context, id string) (bool, error) {
	s.mu.Lock()
	state, ok := s.oauthFlows[id]
	s.mu.Unlock()

	if ok {
		if state.err != nil {
			return false, state.err
		}
		return state.authenticated, nil
	}

	// No active flow — check stored token.
	cfg, err := s.store.Get(id)
	if err != nil {
		return false, err
	}
	if cfg == nil {
		return false, &NotFoundError{Resource: "integration", ID: id}
	}
	return cfg.IsAuthenticated(), nil
}

func (s *integrationService) AvailableTools(_ context.Context) ([]AvailableTool, error) {
	cfgs, err := s.store.List()
	if err != nil {
		return nil, err
	}

	tools := make([]AvailableTool, 0)
	for _, cfg := range cfgs {
		if !cfg.Enabled || !cfg.IsAuthenticated() {
			continue
		}
		for svcName, svc := range cfg.Services {
			if !svc.Enabled {
				continue
			}
			for _, toolName := range svc.Tools {
				tools = append(tools, AvailableTool{
					IntegrationID:   cfg.ID,
					IntegrationName: cfg.Name,
					ToolName:        toolName,
					QualifiedName:   fmt.Sprintf("mcp__%s__%s", cfg.ID, toolName),
					Service:         svcName,
				})
			}
		}
	}
	return tools, nil
}

// ValidateTokenAuth validates token-based authentication for an integration.
// For supported types (e.g. Telegram, Confluence, Jira), it calls the service's API to verify the
// credentials. On success it marks the integration as authenticated, saves it, and reloads its
// MCP server.
func (s *integrationService) ValidateTokenAuth(ctx context.Context, cfg *config.IntegrationConfig) error {
	switch cfg.Type {
	case "confluence":
		return s.validateConfluenceAuth(ctx, cfg)
	case "telegram":
		return s.validateTelegramTokenAuth(ctx, cfg)
	case "jira":
		return s.validateJiraTokenAuth(ctx, cfg)
	case "github":
		return s.validateGitHubPATAuth(ctx, cfg)
	case "slack":
		return s.validateSlackTokenAuth(ctx, cfg)
	default:
		// For other types, validation is not yet implemented. Return nil (unvalidated).
		return nil
	}
}

func (s *integrationService) validateConfluenceAuth(ctx context.Context, cfg *config.IntegrationConfig) error {
	if err := validateAtlassianCredentials(cfg); err != nil {
		return err
	}

	var creds config.AtlassianCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid confluence credentials: " + err.Error()}
	}

	if err := confluence.ValidateCredentials(ctx, creds.SiteURL, creds.Email, creds.APIToken); err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid credentials: " + err.Error()}
	}

	cfg.Auth = json.RawMessage(`{"validated":true}`)
	cfg.UpdatedAt = time.Now().UTC()
	if saveErr := s.store.Save(cfg); saveErr != nil {
		return fmt.Errorf(errFmtSavingValidatedInteg, saveErr)
	}

	s.logger.Info("confluence credentials validated", "id", cfg.ID)
	s.reloadIntegration(cfg.ID)
	return nil
}

func (s *integrationService) validateTelegramTokenAuth(ctx context.Context, cfg *config.IntegrationConfig) error {
	if err := validateTelegramCredentials(cfg); err != nil {
		return err
	}

	var creds config.TelegramCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid telegram credentials: " + err.Error()}
	}

	username, err := telegram.ValidateBotToken(ctx, creds.BotToken)
	if err != nil {
		return &ValidationError{Field: fieldCredentialsBotToken, Message: "invalid bot token: " + err.Error()}
	}

	cfg.Auth = json.RawMessage(fmt.Sprintf(`{"validated":true,"bot_username":%q}`, username))
	cfg.UpdatedAt = time.Now().UTC()
	if saveErr := s.store.Save(cfg); saveErr != nil {
		return fmt.Errorf(errFmtSavingValidatedInteg, saveErr)
	}

	s.logger.Info("telegram bot validated", "id", cfg.ID, "username", username)
	s.reloadIntegration(cfg.ID)
	return nil
}

func (s *integrationService) validateJiraTokenAuth(ctx context.Context, cfg *config.IntegrationConfig) error {
	// Reuse field validation and normalization (strips trailing slash, validates URL scheme).
	if err := validateJiraCredentials(cfg); err != nil {
		return err
	}

	var creds config.AtlassianCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		// ParseCredentials cannot fail here: validateJiraCredentials already succeeded above.
		return fmt.Errorf("parsing jira credentials: %w", err)
	}

	displayName, err := jira.ValidateCredentials(ctx, creds.SiteURL, creds.Email, creds.APIToken)
	if err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid jira credentials: " + err.Error()}
	}

	cfg.Auth = json.RawMessage(fmt.Sprintf(`{"validated":true,"display_name":%q}`, displayName))
	cfg.UpdatedAt = time.Now().UTC()
	if saveErr := s.store.Save(cfg); saveErr != nil {
		return fmt.Errorf(errFmtSavingValidatedInteg, saveErr)
	}

	s.logger.Info("jira integration validated", "id", cfg.ID, "display_name", displayName)
	s.reloadIntegration(cfg.ID)
	return nil
}

func (s *integrationService) validateGitHubPATAuth(ctx context.Context, cfg *config.IntegrationConfig) error {
	if err := validateGitHubCredentials(cfg); err != nil {
		return err
	}

	var creds config.GitHubCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid github credentials: " + err.Error()}
	}

	username, err := githubintegration.ValidatePAT(ctx, creds.PersonalAccessToken)
	if err != nil {
		return &ValidationError{
			Field:   "credentials.personal_access_token",
			Message: "invalid personal access token: " + err.Error(),
		}
	}

	cfg.Auth = json.RawMessage(fmt.Sprintf(`{"validated":true,"username":%q}`, username))
	cfg.UpdatedAt = time.Now().UTC()
	if saveErr := s.store.Save(cfg); saveErr != nil {
		return fmt.Errorf(errFmtSavingValidatedInteg, saveErr)
	}

	s.logger.Info("github integration validated", "id", cfg.ID, "username", username)
	s.reloadIntegration(cfg.ID)
	return nil
}

func (s *integrationService) validateSlackTokenAuth(ctx context.Context, cfg *config.IntegrationConfig) error {
	if err := validateSlackCredentials(cfg); err != nil {
		return err
	}

	var creds config.SlackCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return &ValidationError{Field: "credentials", Message: "invalid slack credentials: " + err.Error()}
	}

	teamName, err := slackintegration.ValidateToken(ctx, creds.BotToken)
	if err != nil {
		return &ValidationError{Field: fieldCredentialsBotToken, Message: "invalid bot token: " + err.Error()}
	}

	cfg.Auth = json.RawMessage(fmt.Sprintf(`{"validated":true,"team_name":%q}`, teamName))
	cfg.UpdatedAt = time.Now().UTC()
	if saveErr := s.store.Save(cfg); saveErr != nil {
		return fmt.Errorf(errFmtSavingValidatedInteg, saveErr)
	}

	s.logger.Info("slack integration validated", "id", cfg.ID, "team", teamName)
	s.reloadIntegration(cfg.ID)
	return nil
}

// reloadIntegration starts or reloads the MCP server for an integration in the background.
func (s *integrationService) reloadIntegration(id string) {
	go func() {
		if reloadErr := s.registry.Reload(context.Background(), id); reloadErr != nil {
			s.logger.Warn("failed to start integration server after validation", "id", id, "error", reloadErr)
		}
	}()
}
