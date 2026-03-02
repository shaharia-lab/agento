package messaging

import (
	"context"
	"fmt"
	"log/slog"
	"net/http"
	"sync"
)

// Manager manages the lifecycle of Platform adapters. It starts and stops
// platforms, registers them with the Dispatcher, and routes inbound webhooks
// to the correct adapter.
type Manager struct {
	dispatcher *Dispatcher
	factories  map[string]PlatformFactory // platformType → factory
	platforms  map[string]Platform        // integrationID → running platform
	cancels   map[string]context.CancelFunc
	mu        sync.RWMutex
	logger    *slog.Logger
}

// NewManager creates a Manager backed by the given Dispatcher.
func NewManager(dispatcher *Dispatcher, logger *slog.Logger) *Manager {
	return &Manager{
		dispatcher: dispatcher,
		factories:  make(map[string]PlatformFactory),
		platforms:  make(map[string]Platform),
		cancels:    make(map[string]context.CancelFunc),
		logger:     logger,
	}
}

// RegisterFactory registers a PlatformFactory for a given platform type.
// This is called once at startup for each supported platform type.
func (m *Manager) RegisterFactory(platformType string, factory PlatformFactory) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.factories[platformType] = factory
}

// StartPlatform creates and starts a platform adapter for the given integration.
// credentials is the parsed credential map (e.g. {"bot_token": "123:ABC"}).
func (m *Manager) StartPlatform(
	parentCtx context.Context,
	platformType, integrationID string,
	credentials map[string]string,
) error {
	m.mu.Lock()
	factory, ok := m.factories[platformType]
	m.mu.Unlock()
	if !ok {
		return fmt.Errorf("no factory registered for platform type %q", platformType)
	}

	// Create the adapter with the dispatcher as its message handler.
	platform, err := factory(integrationID, credentials, m.dispatcher)
	if err != nil {
		return fmt.Errorf("creating %s platform: %w", platformType, err)
	}

	ctx, cancel := context.WithCancel(parentCtx)
	if startErr := platform.Start(ctx); startErr != nil {
		cancel()
		return fmt.Errorf("starting %s platform: %w", platformType, startErr)
	}

	m.mu.Lock()
	// Stop any existing platform for this integration.
	if existingCancel, exists := m.cancels[integrationID]; exists {
		existingCancel()
		delete(m.platforms, integrationID)
	}
	m.platforms[integrationID] = platform
	m.cancels[integrationID] = cancel
	m.mu.Unlock()

	m.dispatcher.RegisterPlatform(platform)

	m.logger.Info("messaging platform started",
		"type", platformType,
		"integration_id", integrationID,
	)
	return nil
}

// StopPlatform stops and removes the platform adapter for the given integration.
func (m *Manager) StopPlatform(integrationID string) {
	m.mu.Lock()
	platform, ok := m.platforms[integrationID]
	cancel, hasCancel := m.cancels[integrationID]
	if ok {
		delete(m.platforms, integrationID)
	}
	if hasCancel {
		delete(m.cancels, integrationID)
	}
	m.mu.Unlock()

	if hasCancel {
		cancel()
	}
	if ok {
		if err := platform.Stop(); err != nil {
			m.logger.Warn("error stopping platform", "id", integrationID, "error", err)
		}
		m.dispatcher.UnregisterPlatform(integrationID)
		m.logger.Info("messaging platform stopped", "integration_id", integrationID)
	}
}

// StopAll stops all running platform adapters.
func (m *Manager) StopAll() {
	m.mu.RLock()
	ids := make([]string, 0, len(m.platforms))
	for id := range m.platforms {
		ids = append(ids, id)
	}
	m.mu.RUnlock()

	for _, id := range ids {
		m.StopPlatform(id)
	}
}

// HandleWebhook routes an inbound webhook to the correct platform adapter.
// platformType and integrationID are extracted from the URL path by the
// API router.
func (m *Manager) HandleWebhook(platformType, integrationID string, w http.ResponseWriter, r *http.Request) {
	m.mu.RLock()
	platform, ok := m.platforms[integrationID]
	m.mu.RUnlock()

	if !ok {
		m.logger.Warn("webhook for unknown platform",
			"type", platformType,
			"integration_id", integrationID,
		)
		http.Error(w, "platform not found", http.StatusNotFound)
		return
	}

	if platform.Type() != platformType {
		http.Error(w, "platform type mismatch", http.StatusBadRequest)
		return
	}

	platform.HandleWebhook(w, r)
}

// GetPlatform returns a running platform by integration ID, or nil if not found.
func (m *Manager) GetPlatform(integrationID string) (Platform, bool) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	p, ok := m.platforms[integrationID]
	return p, ok
}

// IsRunning checks if a platform adapter is running for the given integration.
func (m *Manager) IsRunning(integrationID string) bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	_, ok := m.platforms[integrationID]
	return ok
}

// Dispatcher returns the underlying Dispatcher.
func (m *Manager) Dispatcher() *Dispatcher {
	return m.dispatcher
}
