package config

import (
	"time"

	"golang.org/x/oauth2"
)

// IntegrationConfig holds the configuration for an external service integration.
type IntegrationConfig struct {
	ID          string                   `json:"id"`
	Name        string                   `json:"name"`
	Type        string                   `json:"type"` // "google"
	Enabled     bool                     `json:"enabled"`
	Credentials GoogleCredentials        `json:"credentials"`
	Auth        *oauth2.Token            `json:"auth,omitempty"`
	Services    map[string]ServiceConfig `json:"services"` // "calendar","gmail","drive"
	CreatedAt   time.Time                `json:"created_at"`
	UpdatedAt   time.Time                `json:"updated_at"`
}

// GoogleCredentials holds the OAuth2 client credentials for a Google integration.
type GoogleCredentials struct {
	ClientID     string `json:"client_id"`
	ClientSecret string `json:"client_secret"`
}

// ServiceConfig configures which tools are enabled for a given Google service.
type ServiceConfig struct {
	Enabled bool     `json:"enabled"`
	Tools   []string `json:"tools"`
}
