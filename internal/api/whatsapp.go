package api

import (
	"context"
	"encoding/json"
	"net/http"
	"time"

	"github.com/go-chi/chi/v5"

	whatsappintegration "github.com/shaharia-lab/agento/internal/integrations/whatsapp"
)

// handleStartWhatsAppPairing begins the QR code pairing flow for a WhatsApp integration.
// POST /api/integrations/{id}/whatsapp/pair
func (s *Server) handleStartWhatsAppPairing(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	cfg, err := s.integrationSvc.Get(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}

	if cfg.Type != "whatsapp" {
		s.writeError(w, http.StatusBadRequest, "integration is not a whatsapp type")
		return
	}

	if s.whatsappPairingMgr == nil {
		s.writeError(w, http.StatusServiceUnavailable, "WhatsApp pairing is not available")
		return
	}

	qrCode, err := s.whatsappPairingMgr.StartPairing(r.Context(), id)
	if err != nil {
		s.logger.Error("WhatsApp pairing failed", "id", id, "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to start pairing")
		return
	}

	s.writeJSON(w, http.StatusOK, map[string]string{"qr_code": qrCode})

	// Start polling for pairing completion in the background.
	// Use a detached context so polling survives the HTTP request, but can
	// still be stopped when the pairing session is cleaned up.
	pairingCtx := s.whatsappPairingMgr.SessionContext(id)
	go s.watchWhatsAppPairing(pairingCtx, id)
}

// handleGetWhatsAppQR returns the current QR code for an active pairing session.
// GET /api/integrations/{id}/whatsapp/qr
func (s *Server) handleGetWhatsAppQR(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	cfg, err := s.integrationSvc.Get(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	if cfg.Type != "whatsapp" {
		s.writeError(w, http.StatusBadRequest, "integration is not a whatsapp type")
		return
	}

	if s.whatsappPairingMgr == nil {
		s.writeError(w, http.StatusServiceUnavailable, "WhatsApp pairing is not available")
		return
	}

	// Check if pairing completed.
	paired, phone, pairingErr := s.whatsappPairingMgr.GetStatus(id)
	if pairingErr != nil {
		s.writeJSON(w, http.StatusOK, map[string]any{
			"status": "error",
			"error":  pairingErr.Error(),
		})
		return
	}
	if paired {
		s.writeJSON(w, http.StatusOK, map[string]any{
			"status": "paired",
			"phone":  phone,
		})
		return
	}

	qrCode, ok := s.whatsappPairingMgr.GetQRCode(id)
	if !ok {
		s.writeJSON(w, http.StatusOK, map[string]any{
			"status": "no_session",
		})
		return
	}

	s.writeJSON(w, http.StatusOK, map[string]any{
		"status":  "pending",
		"qr_code": qrCode,
	})
}

// watchWhatsAppPairing polls the pairing session and saves auth data on success.
// The provided context is canceled when the pairing session is cleaned up or
// the server shuts down, which stops this goroutine.
func (s *Server) watchWhatsAppPairing(ctx context.Context, integrationID string) {
	if s.whatsappPairingMgr == nil {
		return
	}

	ticker := time.NewTicker(2 * time.Second)
	defer ticker.Stop()

	// Poll for up to 3 minutes.
	timeout := time.After(3 * time.Minute)

	for {
		select {
		case <-ctx.Done():
			s.logger.Info("WhatsApp pairing watch canceled", "id", integrationID)
			return
		case <-ticker.C:
			paired, phone, pairingErr := s.whatsappPairingMgr.GetStatus(integrationID)
			if pairingErr != nil {
				s.logger.Warn("WhatsApp pairing error", "id", integrationID, "error", pairingErr)
				s.whatsappPairingMgr.CleanupSession(integrationID)
				return
			}
			if paired {
				if err := s.saveWhatsAppAuth(integrationID, phone); err != nil {
					s.logger.Error("failed to save WhatsApp auth", "id", integrationID, "error", err)
				}
				s.whatsappPairingMgr.CleanupSession(integrationID)
				return
			}
		case <-timeout:
			s.logger.Warn("WhatsApp pairing poll timeout", "id", integrationID)
			s.whatsappPairingMgr.CleanupSession(integrationID)
			return
		}
	}
}

// handleGetWhatsAppStatus returns the live connection state of the WhatsApp client.
// GET /api/integrations/{id}/whatsapp/status
func (s *Server) handleGetWhatsAppStatus(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	cfg, err := s.integrationSvc.Get(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	if cfg.Type != "whatsapp" {
		s.writeError(w, http.StatusBadRequest, "integration is not a whatsapp type")
		return
	}

	connected, loggedIn := whatsappintegration.ConnectionStatus(id)
	s.writeJSON(w, http.StatusOK, map[string]any{
		"connected": connected,
		"logged_in": loggedIn,
	})
}

// handleWhatsAppReconnect triggers a reload of the WhatsApp MCP server, re-establishing
// the WebSocket connection to WhatsApp without requiring re-pairing.
// POST /api/integrations/{id}/whatsapp/reconnect
func (s *Server) handleWhatsAppReconnect(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	cfg, err := s.integrationSvc.Get(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}

	if cfg.Type != "whatsapp" {
		s.writeError(w, http.StatusBadRequest, "not a whatsapp integration")
		return
	}

	if !cfg.IsAuthenticated() {
		s.writeError(w, http.StatusBadRequest, "integration is not yet paired")
		return
	}

	_, err = s.integrationSvc.Update(r.Context(), id, cfg)
	if err != nil {
		s.logger.Error("WhatsApp reconnect failed", "id", id, "error", err)
		s.writeError(w, http.StatusInternalServerError, "reconnect failed")
		return
	}

	s.writeJSON(w, http.StatusOK, map[string]string{"status": "reconnecting"})
}

// saveWhatsAppAuth persists the paired status to the integration's Auth field.
func (s *Server) saveWhatsAppAuth(integrationID, phone string) error {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	cfg, err := s.integrationSvc.Get(ctx, integrationID)
	if err != nil {
		return err
	}

	authData := map[string]any{
		"paired": true,
		"phone":  phone,
	}
	b, err := json.Marshal(authData)
	if err != nil {
		return err
	}
	cfg.Auth = b

	_, err = s.integrationSvc.Update(ctx, integrationID, cfg)
	if err != nil {
		return err
	}

	s.logger.Debug("WhatsApp auth saved", "id", integrationID, "phone", phone)
	return nil
}
