package service

import (
	"context"
	"fmt"
	"log/slog"
	"strings"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/codes"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/integrations/telegram"
	"github.com/shaharia-lab/agento/internal/storage"
)

// TriggerService defines the business logic interface for managing trigger rules and webhooks.
type TriggerService interface {
	// ListRules returns all trigger rules for the given integration.
	ListRules(ctx context.Context, integrationID string) ([]*config.TriggerRule, error)

	// GetRule returns a single trigger rule by ID.
	GetRule(ctx context.Context, id string) (*config.TriggerRule, error)

	// CreateRule creates a new trigger rule for an integration.
	CreateRule(ctx context.Context, rule *config.TriggerRule) (*config.TriggerRule, error)

	// UpdateRule updates an existing trigger rule.
	UpdateRule(ctx context.Context, id string, rule *config.TriggerRule) (*config.TriggerRule, error)

	// DeleteRule removes a trigger rule by ID.
	DeleteRule(ctx context.Context, id string) error

	// RegisterWebhook registers a Telegram webhook for the given integration.
	RegisterWebhook(ctx context.Context, integrationID string) error

	// DeleteWebhook removes the Telegram webhook for the given integration.
	DeleteWebhook(ctx context.Context, integrationID string) error

	// GetWebhookStatus returns the webhook status for an integration.
	GetWebhookStatus(ctx context.Context, integrationID string) (*WebhookStatus, error)

	// RegenerateSecret generates a new webhook secret and re-registers the webhook.
	RegenerateSecret(ctx context.Context, integrationID string) error
}

// WebhookStatus holds the current webhook state for an integration.
type WebhookStatus struct {
	Status    string `json:"status"`     // "active", "inactive", "error"
	URL       string `json:"url"`        // The registered webhook URL
	HasSecret bool   `json:"has_secret"` // Whether a secret is configured (never expose the secret itself)
	Error     string `json:"error"`      // Last error message, if any
}

type triggerService struct {
	triggerStore     storage.TriggerStore
	integrationStore storage.IntegrationStore
	settingsMgr      *config.SettingsManager
	appConfig        *config.AppConfig
	logger           *slog.Logger
}

// NewTriggerService returns a new TriggerService.
func NewTriggerService(
	triggerStore storage.TriggerStore,
	integrationStore storage.IntegrationStore,
	settingsMgr *config.SettingsManager,
	appConfig *config.AppConfig,
	logger *slog.Logger,
) TriggerService {
	return &triggerService{
		triggerStore:     triggerStore,
		integrationStore: integrationStore,
		settingsMgr:      settingsMgr,
		appConfig:        appConfig,
		logger:           logger,
	}
}

func (s *triggerService) ListRules(ctx context.Context, integrationID string) ([]*config.TriggerRule, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "trigger.list_rules")
	defer span.End()

	rules, err := s.triggerStore.ListRules(ctx, integrationID)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("listing trigger rules: %w", err)
	}
	return rules, nil
}

func (s *triggerService) GetRule(ctx context.Context, id string) (*config.TriggerRule, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "trigger.get_rule")
	defer span.End()

	rule, err := s.triggerStore.GetRule(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("getting trigger rule: %w", err)
	}
	if rule == nil {
		return nil, &NotFoundError{Resource: "trigger_rule", ID: id}
	}
	return rule, nil
}

func (s *triggerService) CreateRule(ctx context.Context, rule *config.TriggerRule) (*config.TriggerRule, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "trigger.create_rule")
	defer span.End()

	if err := validateTriggerRule(rule); err != nil {
		return nil, err
	}

	// Verify the integration exists.
	integration, err := s.integrationStore.Get(ctx, rule.IntegrationID)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("looking up integration: %w", err)
	}
	if integration == nil {
		return nil, &NotFoundError{Resource: "integration", ID: rule.IntegrationID}
	}

	if err := s.triggerStore.CreateRule(ctx, rule); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("creating trigger rule: %w", err)
	}

	s.logger.Info("trigger rule created", "id", rule.ID, "integration_id", rule.IntegrationID)
	return rule, nil
}

func (s *triggerService) UpdateRule(
	ctx context.Context, id string, rule *config.TriggerRule,
) (*config.TriggerRule, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "trigger.update_rule")
	defer span.End()

	existing, err := s.triggerStore.GetRule(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("looking up trigger rule: %w", err)
	}
	if existing == nil {
		return nil, &NotFoundError{Resource: "trigger_rule", ID: id}
	}

	rule.ID = id
	rule.IntegrationID = existing.IntegrationID
	rule.CreatedAt = existing.CreatedAt

	if err := validateTriggerRule(rule); err != nil {
		return nil, err
	}

	if err := s.triggerStore.UpdateRule(ctx, rule); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("updating trigger rule: %w", err)
	}

	s.logger.Info("trigger rule updated", "id", id)
	return rule, nil
}

func (s *triggerService) DeleteRule(ctx context.Context, id string) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "trigger.delete_rule")
	defer span.End()

	existing, err := s.triggerStore.GetRule(ctx, id)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf("looking up trigger rule: %w", err)
	}
	if existing == nil {
		return &NotFoundError{Resource: "trigger_rule", ID: id}
	}

	if err := s.triggerStore.DeleteRule(ctx, id); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf("deleting trigger rule: %w", err)
	}

	s.logger.Info("trigger rule deleted", "id", id)
	return nil
}

func (s *triggerService) publicURL() string {
	// Env var takes precedence over settings.
	if s.appConfig.PublicURL != "" {
		return strings.TrimRight(s.appConfig.PublicURL, "/")
	}
	return strings.TrimRight(s.settingsMgr.Get().PublicURL, "/")
}

func (s *triggerService) RegisterWebhook(ctx context.Context, integrationID string) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "trigger.register_webhook")
	defer span.End()

	baseURL := s.publicURL()
	if baseURL == "" {
		return &ValidationError{
			Field:   "public_url",
			Message: "public URL must be configured before registering a webhook",
		}
	}

	integration, err := s.integrationStore.Get(ctx, integrationID)
	if err != nil {
		return fmt.Errorf("looking up integration: %w", err)
	}
	if integration == nil {
		return &NotFoundError{Resource: "integration", ID: integrationID}
	}
	if integration.Type != "telegram" {
		return &ValidationError{Field: "type", Message: "webhooks are only supported for telegram integrations"}
	}

	var creds config.TelegramCredentials
	if err := integration.ParseCredentials(&creds); err != nil {
		return fmt.Errorf("parsing telegram credentials: %w", err)
	}

	// Generate or reuse existing secret.
	secret, _, _, err := s.triggerStore.GetWebhookInfo(ctx, integrationID)
	if err != nil {
		return fmt.Errorf("getting webhook info: %w", err)
	}
	if secret == "" {
		secret, err = telegram.GenerateSecretToken()
		if err != nil {
			return fmt.Errorf("generating secret: %w", err)
		}
	}

	webhookURL := fmt.Sprintf("%s/webhooks/telegram/%s", baseURL, integrationID)

	if regErr := telegram.RegisterWebhook(ctx, creds.BotToken, webhookURL, secret); regErr != nil {
		// Store the error status.
		_ = s.triggerStore.SetWebhookInfo(ctx, integrationID, secret, "error", regErr.Error()) //nolint:errcheck
		return fmt.Errorf("registering telegram webhook: %w", regErr)
	}

	if err := s.triggerStore.SetWebhookInfo(ctx, integrationID, secret, "active", ""); err != nil {
		return fmt.Errorf("saving webhook info: %w", err)
	}

	s.logger.Info("telegram webhook registered", "integration_id", integrationID, "url", webhookURL)
	return nil
}

func (s *triggerService) DeleteWebhook(ctx context.Context, integrationID string) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "trigger.delete_webhook")
	defer span.End()

	integration, err := s.integrationStore.Get(ctx, integrationID)
	if err != nil {
		return fmt.Errorf("looking up integration: %w", err)
	}
	if integration == nil {
		return &NotFoundError{Resource: "integration", ID: integrationID}
	}

	var creds config.TelegramCredentials
	if parseErr := integration.ParseCredentials(&creds); parseErr != nil {
		return fmt.Errorf("parsing telegram credentials: %w", parseErr)
	}

	if delErr := telegram.DeleteWebhook(ctx, creds.BotToken); delErr != nil {
		s.logger.Warn("failed to delete telegram webhook", "integration_id", integrationID, "error", delErr)
	}

	if err := s.triggerStore.SetWebhookInfo(ctx, integrationID, "", "inactive", ""); err != nil {
		return fmt.Errorf("clearing webhook info: %w", err)
	}

	s.logger.Info("telegram webhook deleted", "integration_id", integrationID)
	return nil
}

func (s *triggerService) GetWebhookStatus(ctx context.Context, integrationID string) (*WebhookStatus, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "trigger.get_webhook_status")
	defer span.End()

	secret, status, webhookErr, err := s.triggerStore.GetWebhookInfo(ctx, integrationID)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("getting webhook status: %w", err)
	}

	if status == "" {
		status = "inactive"
	}

	ws := &WebhookStatus{
		Status:    status,
		HasSecret: secret != "",
		Error:     webhookErr,
	}

	if status == "active" {
		baseURL := s.publicURL()
		if baseURL != "" {
			ws.URL = fmt.Sprintf("%s/webhooks/telegram/%s", baseURL, integrationID)
		}
	}

	return ws, nil
}

func (s *triggerService) RegenerateSecret(ctx context.Context, integrationID string) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "trigger.regenerate_secret")
	defer span.End()

	// First delete the existing webhook.
	if err := s.DeleteWebhook(ctx, integrationID); err != nil {
		s.logger.Warn("failed to delete old webhook before regenerating", "error", err)
	}

	// Then re-register with a new secret.
	// Clear old secret so RegisterWebhook generates a new one.
	if err := s.triggerStore.SetWebhookInfo(ctx, integrationID, "", "", ""); err != nil {
		return fmt.Errorf("clearing old secret: %w", err)
	}

	return s.RegisterWebhook(ctx, integrationID)
}

func validateTriggerRule(rule *config.TriggerRule) error {
	if rule.AgentSlug == "" {
		return &ValidationError{Field: "agent_slug", Message: "agent_slug is required"}
	}
	if rule.IntegrationID == "" {
		return &ValidationError{Field: "integration_id", Message: "integration_id is required"}
	}
	return nil
}
