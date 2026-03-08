package whatsapp

import (
	"context"
	"fmt"
	"log/slog"
	"sync"

	"go.mau.fi/whatsmeow"
)

// PairingSession tracks the state of a QR code pairing flow.
type PairingSession struct {
	mu          sync.Mutex
	client      *Client
	currentQR   string
	paired      bool
	phone       string
	err         error
	done        bool
	ctx         context.Context //nolint:containedctx // stored for session lifecycle management
	cancel      context.CancelFunc
	logger      *slog.Logger
	integration string
}

// PairingManager manages concurrent pairing sessions for WhatsApp integrations.
type PairingManager struct {
	mu       sync.Mutex
	sessions map[string]*PairingSession // integration ID -> session
	logger   *slog.Logger
	dataDir  string
}

// NewPairingManager creates a new pairing manager.
func NewPairingManager(dataDir string, logger *slog.Logger) *PairingManager {
	return &PairingManager{
		sessions: make(map[string]*PairingSession),
		logger:   logger,
		dataDir:  dataDir,
	}
}

// StartPairing begins a new QR code pairing session for the given integration.
// If a session is already in progress, it is canceled and replaced.
// Returns the first QR code string (suitable for rendering as a QR image).
func (m *PairingManager) StartPairing(ctx context.Context, integrationID string) (string, error) {
	m.mu.Lock()

	// Cancel any existing session.
	if existing, ok := m.sessions[integrationID]; ok {
		existing.cancelSession()
		delete(m.sessions, integrationID)
	}
	m.mu.Unlock()

	// Detach from the caller's context so the pairing session survives after
	// the HTTP handler returns. Use context.WithoutCancel to inherit values
	// (e.g. tracing) without inheriting the cancellation signal.
	detachedCtx := context.WithoutCancel(ctx)

	client, err := NewClient(detachedCtx, m.dataDir, integrationID, m.logger)
	if err != nil {
		return "", fmt.Errorf("creating whatsapp client: %w", err)
	}

	pairingCtx, cancel := context.WithCancel(detachedCtx)
	session := &PairingSession{
		client:      client,
		ctx:         pairingCtx,
		cancel:      cancel,
		logger:      m.logger,
		integration: integrationID,
	}

	// Start the QR code generation.
	firstQR, err := session.start(pairingCtx)
	if err != nil {
		cancel()
		client.Disconnect()
		return "", fmt.Errorf("starting pairing: %w", err)
	}

	m.mu.Lock()
	m.sessions[integrationID] = session
	m.mu.Unlock()

	return firstQR, nil
}

// GetQRCode returns the current QR code for an active pairing session.
// Returns empty string if no session is active or pairing is complete.
func (m *PairingManager) GetQRCode(integrationID string) (string, bool) {
	m.mu.Lock()
	session, ok := m.sessions[integrationID]
	m.mu.Unlock()

	if !ok {
		return "", false
	}

	session.mu.Lock()
	defer session.mu.Unlock()

	if session.paired || session.done {
		return "", false
	}

	return session.currentQR, session.currentQR != ""
}

// GetStatus returns the pairing status for an integration.
func (m *PairingManager) GetStatus(integrationID string) (paired bool, phone string, pairingErr error) {
	m.mu.Lock()
	session, ok := m.sessions[integrationID]
	m.mu.Unlock()

	if !ok {
		return false, "", nil
	}

	session.mu.Lock()
	defer session.mu.Unlock()

	return session.paired, session.phone, session.err
}

// SessionContext returns the context for an active pairing session.
// If no session exists, it returns a canceled context.
func (m *PairingManager) SessionContext(integrationID string) context.Context {
	m.mu.Lock()
	session, ok := m.sessions[integrationID]
	m.mu.Unlock()

	if !ok || session == nil {
		ctx, cancel := context.WithCancel(context.Background())
		cancel()
		return ctx
	}

	return session.ctx
}

// Shutdown cancels all active pairing sessions. Call this on server shutdown.
func (m *PairingManager) Shutdown() {
	m.mu.Lock()
	defer m.mu.Unlock()

	for id, session := range m.sessions {
		session.cancelSession()
		delete(m.sessions, id)
	}
}

// CleanupSession removes a completed pairing session.
func (m *PairingManager) CleanupSession(integrationID string) {
	m.mu.Lock()
	defer m.mu.Unlock()

	if session, ok := m.sessions[integrationID]; ok {
		session.cancelSession()
		delete(m.sessions, integrationID)
	}
}

// cancelSession stops the pairing session.
func (s *PairingSession) cancelSession() {
	if s.cancel != nil {
		s.cancel()
	}
	if s.client != nil {
		s.client.Disconnect()
	}
}

// start begins the QR code pairing flow and returns the first QR code.
func (s *PairingSession) start(ctx context.Context) (string, error) {
	qrChan, err := s.client.WM().GetQRChannel(ctx)
	if err != nil {
		return "", fmt.Errorf("getting QR channel: %w", err)
	}

	// Connect in the background (this triggers QR code generation).
	if err := s.client.Connect(); err != nil {
		return "", fmt.Errorf("connecting for pairing: %w", err)
	}

	// Wait for the first QR code event.
	for evt := range qrChan {
		if evt.Event == "code" {
			s.mu.Lock()
			s.currentQR = evt.Code
			s.mu.Unlock()

			// Start background goroutine to process remaining events after first QR.
			go s.processQREvents(ctx, qrChan)
			return evt.Code, nil
		}
		if evt == whatsmeow.QRChannelSuccess {
			s.mu.Lock()
			s.paired = true
			s.done = true
			if s.client.WM().Store.ID != nil {
				s.phone = s.client.WM().Store.ID.User
			}
			s.mu.Unlock()
			return "", fmt.Errorf("device already paired")
		}
		if evt == whatsmeow.QRChannelTimeout {
			s.mu.Lock()
			s.err = fmt.Errorf("QR code pairing timed out")
			s.done = true
			s.mu.Unlock()
			return "", s.err
		}
	}

	return "", fmt.Errorf("QR channel closed without producing a code")
}

// processQREvents continues processing QR events in the background after
// the first code has been returned to the caller.
func (s *PairingSession) processQREvents(ctx context.Context, qrChan <-chan whatsmeow.QRChannelItem) {
	for {
		select {
		case <-ctx.Done():
			s.markDone(ctx.Err())
			return
		case evt, ok := <-qrChan:
			if !ok {
				s.markDone(nil)
				return
			}
			if done := s.handleQREvent(evt); done {
				return
			}
		}
	}
}

// markDone sets the session as done with an optional error.
func (s *PairingSession) markDone(err error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.done = true
	if err != nil && s.err == nil {
		s.err = err
	}
}

// handleQREvent processes a single QR channel event. Returns true if the event
// loop should exit.
func (s *PairingSession) handleQREvent(evt whatsmeow.QRChannelItem) bool {
	switch {
	case evt.Event == "code":
		s.mu.Lock()
		s.currentQR = evt.Code
		s.mu.Unlock()
		s.logger.Debug("new WhatsApp QR code generated", "integration", s.integration)
		return false
	case evt == whatsmeow.QRChannelSuccess:
		s.mu.Lock()
		s.paired = true
		s.done = true
		if s.client.WM().Store.ID != nil {
			s.phone = s.client.WM().Store.ID.User
		}
		s.mu.Unlock()
		s.logger.Info("WhatsApp pairing successful", "integration", s.integration, "phone", s.phone)
		return true
	case evt == whatsmeow.QRChannelTimeout:
		s.markDone(fmt.Errorf("QR code pairing timed out"))
		s.logger.Warn("WhatsApp QR pairing timed out", "integration", s.integration)
		return true
	case evt.Event == "error":
		s.markDone(evt.Error)
		s.logger.Warn("WhatsApp pairing error", "integration", s.integration, "error", evt.Error)
		return true
	default:
		return false
	}
}
