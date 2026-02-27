// Package service implements the business logic layer between HTTP handlers
// and the storage/agent packages. All interfaces are designed for easy mocking
// in tests.
package service

import (
	"context"
	"fmt"
	"log/slog"
	"regexp"
	"strings"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/storage"
)

var slugRE = regexp.MustCompile(`^[a-z0-9]+(?:-[a-z0-9]+)*$`)

// AgentService defines the business logic interface for managing agents.
type AgentService interface {
	// List returns all stored agents.
	List(ctx context.Context) ([]*config.AgentConfig, error)

	// Get returns the agent identified by slug, or nil if not found.
	Get(ctx context.Context, slug string) (*config.AgentConfig, error)

	// Create validates and persists a new agent.
	Create(ctx context.Context, agent *config.AgentConfig) (*config.AgentConfig, error)

	// Update replaces an existing agent. Returns an error if the agent does not exist.
	Update(ctx context.Context, slug string, agent *config.AgentConfig) (*config.AgentConfig, error)

	// Delete removes an agent by slug. Returns an error if not found.
	Delete(ctx context.Context, slug string) error
}

// agentService is the default implementation of AgentService.
type agentService struct {
	repo   storage.AgentStore
	logger *slog.Logger
}

// NewAgentService returns a new AgentService backed by the given AgentStore.
func NewAgentService(repo storage.AgentStore, logger *slog.Logger) AgentService {
	return &agentService{repo: repo, logger: logger}
}

func (s *agentService) List(_ context.Context) ([]*config.AgentConfig, error) {
	agents, err := s.repo.List()
	if err != nil {
		return nil, fmt.Errorf("listing agents: %w", err)
	}
	return agents, nil
}

func (s *agentService) Get(_ context.Context, slug string) (*config.AgentConfig, error) {
	agent, err := s.repo.Get(slug)
	if err != nil {
		return nil, fmt.Errorf("getting agent %q: %w", slug, err)
	}
	return agent, nil
}

func (s *agentService) Create(_ context.Context, agent *config.AgentConfig) (*config.AgentConfig, error) {
	if agent.Name == "" {
		return nil, &ValidationError{Field: "name", Message: "name is required"}
	}
	if agent.Slug == "" {
		agent.Slug = toSlug(agent.Name)
	}
	if !slugRE.MatchString(agent.Slug) {
		return nil, &ValidationError{
			Field:   "slug",
			Message: fmt.Sprintf("invalid slug %q: use lowercase letters, digits and hyphens", agent.Slug),
		}
	}
	if agent.Model == "" {
		agent.Model = "claude-sonnet-4-6"
	}
	if agent.Thinking == "" {
		agent.Thinking = "adaptive"
	}
	switch agent.PermissionMode {
	case "", "bypass", "default":
		// valid
	default:
		return nil, &ValidationError{Field: "permission_mode", Message: "must be bypass or default"}
	}

	existing, err := s.repo.Get(agent.Slug)
	if err != nil {
		return nil, fmt.Errorf("checking slug uniqueness: %w", err)
	}
	if existing != nil {
		return nil, &ConflictError{Resource: "agent", ID: agent.Slug}
	}

	if err := s.repo.Save(agent); err != nil {
		return nil, fmt.Errorf("saving agent: %w", err)
	}

	s.logger.Info("agent created", "slug", agent.Slug)
	return agent, nil
}

func (s *agentService) Update(_ context.Context, slug string, agent *config.AgentConfig) (*config.AgentConfig, error) {
	existing, err := s.repo.Get(slug)
	if err != nil {
		return nil, fmt.Errorf("looking up agent: %w", err)
	}
	if existing == nil {
		return nil, &NotFoundError{Resource: "agent", ID: slug}
	}

	if agent.Name == "" {
		return nil, &ValidationError{Field: "name", Message: "name is required"}
	}

	// Slug is the stable identifier — it cannot be changed via an update.
	agent.Slug = slug
	if agent.Model == "" {
		agent.Model = "claude-sonnet-4-6"
	}
	switch agent.PermissionMode {
	case "", "bypass", "default":
		// valid
	default:
		return nil, &ValidationError{Field: "permission_mode", Message: "must be bypass or default"}
	}
	if agent.Thinking == "" {
		agent.Thinking = "adaptive"
	}

	if err := s.repo.Save(agent); err != nil {
		return nil, fmt.Errorf("saving agent: %w", err)
	}

	s.logger.Info("agent updated", "slug", slug)
	return agent, nil
}

func (s *agentService) Delete(_ context.Context, slug string) error {
	if err := s.repo.Delete(slug); err != nil {
		return fmt.Errorf("deleting agent %q: %w", slug, err)
	}
	s.logger.Info("agent deleted", "slug", slug)
	return nil
}

// toSlug converts a human-readable name into a URL-safe slug.
func toSlug(name string) string {
	lower := strings.ToLower(name)
	var result []byte
	prevHyphen := false
	for i := 0; i < len(lower); i++ {
		c := lower[i]
		if (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') {
			result = append(result, c)
			prevHyphen = false
		} else if !prevHyphen && len(result) > 0 {
			result = append(result, '-')
			prevHyphen = true
		}
	}
	for len(result) > 0 && result[len(result)-1] == '-' {
		result = result[:len(result)-1]
	}
	return string(result)
}
