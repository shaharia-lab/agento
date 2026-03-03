package storage

import (
	"context"

	"github.com/shaharia-lab/agento/internal/config"
)

// IntegrationStore defines the interface for integration persistence.
type IntegrationStore interface {
	List(ctx context.Context) ([]*config.IntegrationConfig, error)
	Get(ctx context.Context, id string) (*config.IntegrationConfig, error)
	Save(ctx context.Context, cfg *config.IntegrationConfig) error
	Delete(ctx context.Context, id string) error
}
