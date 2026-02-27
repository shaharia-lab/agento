package storage

import "github.com/shaharia-lab/agento/internal/config"

// IntegrationStore defines the interface for integration persistence.
type IntegrationStore interface {
	List() ([]*config.IntegrationConfig, error)
	Get(id string) (*config.IntegrationConfig, error)
	Save(cfg *config.IntegrationConfig) error
	Delete(id string) error
}
