package config

import (
	"bytes"
	"encoding/json"
	"fmt"
	"time"

	"golang.org/x/oauth2"
)

// IntegrationConfig holds the configuration for an external service integration.
type IntegrationConfig struct {
	ID          string                   `json:"id"`
	Name        string                   `json:"name"`
	Type        string                   `json:"type"` // "google", "telegram", "jira", "confluence", "slack", "github"
	Enabled     bool                     `json:"enabled"`
	Credentials json.RawMessage          `json:"credentials"`
	Auth        json.RawMessage          `json:"auth,omitempty"`
	Services    map[string]ServiceConfig `json:"services"` // "calendar","gmail","drive"
	CreatedAt   time.Time                `json:"created_at"`
	UpdatedAt   time.Time                `json:"updated_at"`
}

// ParseCredentials unmarshals the Credentials field into the given typed struct.
func (c *IntegrationConfig) ParseCredentials(out any) error {
	if len(c.Credentials) == 0 {
		return fmt.Errorf("credentials are empty")
	}
	return json.Unmarshal(c.Credentials, out)
}

// SetCredentials marshals the given value and stores it in the Credentials field.
func (c *IntegrationConfig) SetCredentials(v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return fmt.Errorf("marshaling credentials: %w", err)
	}
	c.Credentials = b
	return nil
}

// IsAuthenticated checks if the Auth field contains a non-empty, non-null JSON value.
func (c *IntegrationConfig) IsAuthenticated() bool {
	return len(c.Auth) > 0 && !bytes.Equal(c.Auth, []byte("null"))
}

// ParseOAuthToken parses the Auth field as an oauth2.Token.
func (c *IntegrationConfig) ParseOAuthToken() (*oauth2.Token, error) {
	if !c.IsAuthenticated() {
		return nil, fmt.Errorf("no auth token stored")
	}
	var tok oauth2.Token
	if err := json.Unmarshal(c.Auth, &tok); err != nil {
		return nil, fmt.Errorf("parsing oauth token: %w", err)
	}
	return &tok, nil
}

// SetOAuthToken marshals the given token and stores it in the Auth field.
func (c *IntegrationConfig) SetOAuthToken(tok *oauth2.Token) error {
	b, err := json.Marshal(tok)
	if err != nil {
		return fmt.Errorf("marshaling oauth token: %w", err)
	}
	c.Auth = b
	return nil
}

// GoogleCredentials holds the OAuth2 client credentials for a Google integration.
type GoogleCredentials struct {
	ClientID     string `json:"client_id"`
	ClientSecret string `json:"client_secret"`
}

// TelegramCredentials holds credentials for a Telegram bot integration.
type TelegramCredentials struct {
	BotToken string `json:"bot_token"`
}

// AtlassianCredentials holds credentials shared by Jira and Confluence integrations.
type AtlassianCredentials struct {
	SiteURL  string `json:"site_url"`
	Email    string `json:"email"`
	APIToken string `json:"api_token"`
}

// SlackCredentials holds credentials for a Slack integration.
type SlackCredentials struct {
	AuthMode     string `json:"auth_mode"` // "bot_token" or "oauth"
	BotToken     string `json:"bot_token,omitempty"`
	ClientID     string `json:"client_id,omitempty"`
	ClientSecret string `json:"client_secret,omitempty"`
}

// GitHubCredentials holds credentials for a GitHub integration.
type GitHubCredentials struct {
	AuthMode            string `json:"auth_mode"` // "pat", "oauth", or "app"
	PersonalAccessToken string `json:"personal_access_token,omitempty"`
	ClientID            string `json:"client_id,omitempty"`
	ClientSecret        string `json:"client_secret,omitempty"`
	AppID               string `json:"app_id,omitempty"`
	PrivateKey          string `json:"private_key,omitempty"`
	InstallationID      string `json:"installation_id,omitempty"`
}

// ServiceConfig configures which tools are enabled for a given Google service.
type ServiceConfig struct {
	Enabled bool     `json:"enabled"`
	Tools   []string `json:"tools"`
}
