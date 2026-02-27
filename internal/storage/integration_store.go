package storage

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/shaharia-lab/agento/internal/config"
)

// IntegrationStore defines the interface for integration persistence.
type IntegrationStore interface {
	List() ([]*config.IntegrationConfig, error)
	Get(id string) (*config.IntegrationConfig, error)
	Save(cfg *config.IntegrationConfig) error
	Delete(id string) error
}

// FSIntegrationStore implements IntegrationStore on the local filesystem.
// Each integration is stored as a JSON file: <dir>/<id>.json
type FSIntegrationStore struct {
	dir string
}

// NewFSIntegrationStore creates an FSIntegrationStore rooted at dir.
func NewFSIntegrationStore(dir string) *FSIntegrationStore {
	return &FSIntegrationStore{dir: dir}
}

// List returns all integration configs stored on the filesystem.
func (s *FSIntegrationStore) List() ([]*config.IntegrationConfig, error) {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		if os.IsNotExist(err) {
			return []*config.IntegrationConfig{}, nil
		}
		return nil, fmt.Errorf("reading integrations dir: %w", err)
	}

	var integrations []*config.IntegrationConfig
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}
		id := strings.TrimSuffix(entry.Name(), ".json")
		cfg, err := s.Get(id)
		if err != nil {
			return nil, err
		}
		if cfg != nil {
			integrations = append(integrations, cfg)
		}
	}
	if integrations == nil {
		integrations = []*config.IntegrationConfig{}
	}
	return integrations, nil
}

// Get returns the integration config for the given id, or nil if not found.
func (s *FSIntegrationStore) Get(id string) (*config.IntegrationConfig, error) {
	data, err := os.ReadFile(filepath.Join(s.dir, id+".json")) //nolint:gosec // path constructed from admin-configured data dir
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("reading integration %q: %w", id, err)
	}

	var cfg config.IntegrationConfig
	if err := json.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parsing integration %q: %w", id, err)
	}
	return &cfg, nil
}

// Save persists the integration config to the filesystem.
func (s *FSIntegrationStore) Save(cfg *config.IntegrationConfig) error {
	if cfg.ID == "" {
		return fmt.Errorf("integration id is required")
	}
	data, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling integration %q: %w", cfg.ID, err)
	}
	path := filepath.Join(s.dir, cfg.ID+".json")
	if err := os.WriteFile(path, data, 0600); err != nil {
		return fmt.Errorf("writing integration %q: %w", cfg.ID, err)
	}
	return nil
}

// Delete removes the integration config file for the given id.
func (s *FSIntegrationStore) Delete(id string) error {
	path := filepath.Join(s.dir, id+".json")
	if err := os.Remove(path); err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("integration %q not found", id)
		}
		return fmt.Errorf("deleting integration %q: %w", id, err)
	}
	return nil
}
