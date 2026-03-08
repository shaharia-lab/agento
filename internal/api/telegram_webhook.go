package api

import (
	"encoding/json"
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/storage"
	"github.com/shaharia-lab/agento/internal/trigger"
)

// TelegramWebhookHandler handles inbound Telegram webhook requests.
// It is mounted outside the /api prefix on the main router.
type TelegramWebhookHandler struct {
	triggerStore     storage.TriggerStore
	integrationStore storage.IntegrationStore
	dispatcher       *trigger.Dispatcher
}

// NewTelegramWebhookHandler creates a handler for inbound Telegram webhooks.
func NewTelegramWebhookHandler(
	triggerStore storage.TriggerStore,
	integrationStore storage.IntegrationStore,
	dispatcher *trigger.Dispatcher,
) *TelegramWebhookHandler {
	return &TelegramWebhookHandler{
		triggerStore:     triggerStore,
		integrationStore: integrationStore,
		dispatcher:       dispatcher,
	}
}

// Mount registers the webhook route on the given router.
func (h *TelegramWebhookHandler) Mount(r chi.Router) {
	r.Post("/webhooks/telegram/{id}", h.handleInbound)
}

func (h *TelegramWebhookHandler) handleInbound(w http.ResponseWriter, r *http.Request) {
	integrationID := chi.URLParam(r, "id")

	// Verify the secret token from the X-Telegram-Bot-Api-Secret-Token header.
	headerSecret := r.Header.Get("X-Telegram-Bot-Api-Secret-Token")

	storedSecret, status, _, err := h.triggerStore.GetWebhookInfo(r.Context(), integrationID)
	if err != nil || status != "active" || storedSecret == "" {
		// Return 200 to prevent Telegram from retrying, but do nothing.
		w.WriteHeader(http.StatusOK)
		return
	}

	if headerSecret != storedSecret {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}

	// Verify integration is enabled.
	integration, err := h.integrationStore.Get(r.Context(), integrationID)
	if err != nil || integration == nil || !integration.Enabled {
		w.WriteHeader(http.StatusOK)
		return
	}

	// Parse the update.
	var update trigger.TelegramUpdate
	if json.NewDecoder(r.Body).Decode(&update) != nil {
		w.WriteHeader(http.StatusOK)
		return
	}

	// Extract bot token.
	var creds config.TelegramCredentials
	if parseErr := integration.ParseCredentials(&creds); parseErr != nil {
		w.WriteHeader(http.StatusOK)
		return
	}

	// Return 200 immediately to Telegram, process in background.
	w.WriteHeader(http.StatusOK)

	// Dispatch asynchronously.
	h.dispatcher.HandleTelegramUpdate(integrationID, creds.BotToken, update)
}
