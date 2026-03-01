package api

import (
	"encoding/json"
	"net/http"
	"strconv"

	"github.com/shaharia-lab/agento/internal/notification"
)

// handleGetNotificationSettings returns the current notification settings.
// The SMTP password is masked before returning.
func (s *Server) handleGetNotificationSettings(w http.ResponseWriter, r *http.Request) {
	settings, err := s.notificationSvc.GetSettings()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to load notification settings")
		return
	}
	writeJSON(w, http.StatusOK, settings)
}

// handleUpdateNotificationSettings persists new notification settings.
// If the submitted password is the mask sentinel ("***"), the existing password is kept.
func (s *Server) handleUpdateNotificationSettings(w http.ResponseWriter, r *http.Request) {
	var incoming notification.NotificationSettings
	if err := json.NewDecoder(r.Body).Decode(&incoming); err != nil {
		writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	if err := s.notificationSvc.UpdateSettings(&incoming); err != nil {
		writeError(w, http.StatusInternalServerError, "failed to save notification settings")
		return
	}

	// Return the saved settings (with masked password).
	settings, err := s.notificationSvc.GetSettings()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to reload notification settings")
		return
	}
	writeJSON(w, http.StatusOK, settings)
}

// handleTestNotification sends a test email using the current notification settings.
func (s *Server) handleTestNotification(w http.ResponseWriter, r *http.Request) {
	if err := s.notificationSvc.TestNotification(r.Context()); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

// handleListNotificationLog returns recent notification delivery log entries.
// Accepts an optional ?limit=N query parameter (default 50).
func (s *Server) handleListNotificationLog(w http.ResponseWriter, r *http.Request) {
	limit := 50
	if l := r.URL.Query().Get("limit"); l != "" {
		if n, err := strconv.Atoi(l); err == nil && n > 0 {
			limit = n
		}
	}

	entries, err := s.notificationSvc.ListLog(r.Context(), limit)
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to list notification log")
		return
	}
	writeJSON(w, http.StatusOK, entries)
}
