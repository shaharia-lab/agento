package service

import (
	"context"
	"fmt"
	"log/slog"
	"sync"
	"time"

	"github.com/google/uuid"
	"golang.org/x/oauth2"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/integrations"
	"github.com/shaharia-lab/agento/internal/integrations/google"
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
}

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

	// parentCtx is used to derive child contexts for callback servers.
	parentCtx context.Context //nolint:containedctx
}

// NewIntegrationService returns a new IntegrationService.
func NewIntegrationService(
	store storage.IntegrationStore,
	registry *integrations.IntegrationRegistry,
	logger *slog.Logger,
	parentCtx context.Context,
) IntegrationService {
	return &integrationService{
		store:      store,
		registry:   registry,
		logger:     logger,
		oauthFlows: make(map[string]*oauthState),
		parentCtx:  parentCtx,
	}
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
	if cfg.Credentials.ClientID == "" {
		return nil, &ValidationError{Field: "credentials.client_id", Message: "client_id is required"}
	}
	if cfg.Credentials.ClientSecret == "" {
		return nil, &ValidationError{Field: "credentials.client_secret", Message: "client_secret is required"}
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
	if cfg.Auth == nil {
		cfg.Auth = existing.Auth
	}

	if err := s.store.Save(cfg); err != nil {
		return nil, fmt.Errorf("saving integration: %w", err)
	}

	// Reload the in-process MCP server with the new config.
	if reloadErr := s.registry.Reload(ctx, id); reloadErr != nil {
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

func (s *integrationService) StartOAuth(_ context.Context, id string) (string, error) {
	cfg, err := s.store.Get(id)
	if err != nil {
		return "", err
	}
	if cfg == nil {
		return "", &NotFoundError{Resource: "integration", ID: id}
	}

	port, err := google.FreePort()
	if err != nil {
		return "", fmt.Errorf("finding free port: %w", err)
	}

	state := &oauthState{}
	s.mu.Lock()
	s.oauthFlows[id] = state
	s.mu.Unlock()

	authURL := google.BuildAuthURL(cfg, port)

	callbackCtx, cancelCallback := context.WithTimeout(s.parentCtx, 10*time.Minute)
	defer func() {
		// cancelCallback is a no-op if already called by onToken.
		cancelCallback()
	}()

	onToken := func(tok *oauth2.Token, tokErr error) {
		defer cancelCallback()
		s.handleOAuthToken(id, state, tok, tokErr)
	}

	if err := google.StartCallbackServer(callbackCtx, port, cfg, onToken); err != nil {
		return "", fmt.Errorf("starting callback server: %w", err)
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
	latestCfg.Auth = tok
	latestCfg.UpdatedAt = time.Now().UTC()
	if saveErr := s.store.Save(latestCfg); saveErr != nil {
		state.err = fmt.Errorf("saving token: %w", saveErr)
		return
	}

	state.authenticated = true
	s.logger.Info("OAuth completed, starting integration server", "id", id)

	// Start the MCP server for this newly-authenticated integration.
	go func() {
		if startErr := s.registry.Reload(s.parentCtx, id); startErr != nil {
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
	return cfg.Auth != nil, nil
}

func (s *integrationService) AvailableTools(_ context.Context) ([]AvailableTool, error) {
	cfgs, err := s.store.List()
	if err != nil {
		return nil, err
	}

	var tools []AvailableTool
	for _, cfg := range cfgs {
		if !cfg.Enabled || cfg.Auth == nil {
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
