// Package telegram provides a Telegram bot adapter that implements the
// messaging.Platform interface for bidirectional chat.
package telegram

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strconv"
	"time"

	"github.com/shaharia-lab/agento/internal/messaging"
)

// apiBaseURL is the Telegram Bot API base. Variable so tests can override.
var apiBaseURL = "https://api.telegram.org"

// httpClient used for outbound Telegram API calls.
var httpClient = &http.Client{Timeout: 60 * time.Second}

// Adapter implements messaging.Platform for Telegram.
type Adapter struct {
	integrationID string
	botToken      string
	handler       messaging.MessageHandler
	logger        *slog.Logger
	cancel        context.CancelFunc
}

// NewAdapter creates a Telegram messaging adapter.
func NewAdapter(integrationID, botToken string, handler messaging.MessageHandler, logger *slog.Logger) *Adapter {
	return &Adapter{
		integrationID: integrationID,
		botToken:      botToken,
		handler:       handler,
		logger:        logger,
	}
}

// NewFactory returns a messaging.PlatformFactory for Telegram.
func NewFactory(logger *slog.Logger) messaging.PlatformFactory {
	return func(integrationID string, credentials map[string]string, handler messaging.MessageHandler) (messaging.Platform, error) {
		botToken := credentials["bot_token"]
		if botToken == "" {
			return nil, fmt.Errorf("missing bot_token in credentials")
		}
		return NewAdapter(integrationID, botToken, handler, logger), nil
	}
}

// ID returns the integration ID.
func (a *Adapter) ID() string { return a.integrationID }

// Type returns "telegram".
func (a *Adapter) Type() string { return "telegram" }

// Start initialises the adapter. Currently a no-op since we use webhooks.
func (a *Adapter) Start(ctx context.Context) error {
	_, cancel := context.WithCancel(ctx)
	a.cancel = cancel
	a.logger.Info("telegram messaging adapter started", "integration_id", a.integrationID)
	return nil
}

// Stop gracefully shuts down the adapter.
func (a *Adapter) Stop() error {
	if a.cancel != nil {
		a.cancel()
	}
	a.logger.Info("telegram messaging adapter stopped", "integration_id", a.integrationID)
	return nil
}

// telegramUpdate represents the Telegram Update object received via webhook.
type telegramUpdate struct {
	UpdateID int64           `json:"update_id"`
	Message  *telegramMsg    `json:"message,omitempty"`
}

type telegramMsg struct {
	MessageID int64        `json:"message_id"`
	From      *telegramUser `json:"from,omitempty"`
	Chat      telegramChat `json:"chat"`
	Date      int64        `json:"date"`
	Text      string       `json:"text"`
}

type telegramUser struct {
	ID        int64  `json:"id"`
	IsBot     bool   `json:"is_bot"`
	FirstName string `json:"first_name"`
	LastName  string `json:"last_name,omitempty"`
	Username  string `json:"username,omitempty"`
}

type telegramChat struct {
	ID   int64  `json:"id"`
	Type string `json:"type"`
}

// HandleWebhook processes an inbound Telegram webhook request.
func (a *Adapter) HandleWebhook(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	body, err := io.ReadAll(io.LimitReader(r.Body, 10*1024*1024))
	if err != nil {
		a.logger.Error("failed to read webhook body", "error", err)
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}

	var update telegramUpdate
	if err := json.Unmarshal(body, &update); err != nil {
		a.logger.Error("failed to parse webhook update", "error", err)
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}

	// Acknowledge the webhook immediately — process the message asynchronously.
	w.WriteHeader(http.StatusOK)

	if update.Message == nil || update.Message.Text == "" {
		return
	}

	msg := a.toInboundMessage(update)
	go func() {
		ctx := context.Background()
		if handleErr := a.handler.HandleMessage(ctx, msg); handleErr != nil {
			a.logger.Error("failed to handle inbound message",
				"chat_id", update.Message.Chat.ID,
				"error", handleErr,
			)
		}
	}()
}

// toInboundMessage converts a Telegram update to a generic InboundMessage.
func (a *Adapter) toInboundMessage(update telegramUpdate) messaging.InboundMessage {
	tgMsg := update.Message
	channelID := strconv.FormatInt(tgMsg.Chat.ID, 10)
	messageID := strconv.FormatInt(tgMsg.MessageID, 10)

	displayName := ""
	userID := ""
	if tgMsg.From != nil {
		userID = strconv.FormatInt(tgMsg.From.ID, 10)
		displayName = tgMsg.From.FirstName
		if tgMsg.From.LastName != "" {
			displayName += " " + tgMsg.From.LastName
		}
		if displayName == "" {
			displayName = tgMsg.From.Username
		}
	}

	return messaging.InboundMessage{
		PlatformType:    "telegram",
		PlatformID:      a.integrationID,
		ChannelID:       channelID,
		MessageID:       messageID,
		UserID:          userID,
		UserDisplayName: displayName,
		Content:         tgMsg.Text,
		Timestamp:       time.Unix(tgMsg.Date, 0),
		Metadata: map[string]string{
			"update_id": strconv.FormatInt(update.UpdateID, 10),
			"chat_type": tgMsg.Chat.Type,
		},
	}
}

// SendMessage sends a text message to a Telegram chat.
func (a *Adapter) SendMessage(ctx context.Context, msg messaging.OutboundMessage) error {
	chatID, err := strconv.ParseInt(msg.ChannelID, 10, 64)
	if err != nil {
		return fmt.Errorf("invalid chat_id %q: %w", msg.ChannelID, err)
	}

	// Telegram has a 4096 character limit per message. Split if needed.
	chunks := splitMessage(msg.Content, 4096)
	for _, chunk := range chunks {
		payload := map[string]any{
			"chat_id":    chatID,
			"text":       chunk,
			"parse_mode": "Markdown",
		}

		if msg.ReplyToMessageID != "" {
			if replyID, parseErr := strconv.ParseInt(msg.ReplyToMessageID, 10, 64); parseErr == nil {
				payload["reply_to_message_id"] = replyID
			}
		}

		if sendErr := a.callAPI(ctx, "sendMessage", payload); sendErr != nil {
			return sendErr
		}
	}
	return nil
}

// SendTypingIndicator sends a "typing" chat action.
func (a *Adapter) SendTypingIndicator(ctx context.Context, channelID string) error {
	chatID, err := strconv.ParseInt(channelID, 10, 64)
	if err != nil {
		return fmt.Errorf("invalid chat_id %q: %w", channelID, err)
	}
	return a.callAPI(ctx, "sendChatAction", map[string]any{
		"chat_id": chatID,
		"action":  "typing",
	})
}

// callAPI makes a POST request to the Telegram Bot API.
func (a *Adapter) callAPI(ctx context.Context, method string, payload any) error {
	body, err := json.Marshal(payload)
	if err != nil {
		return fmt.Errorf("marshaling request: %w", err)
	}

	url := fmt.Sprintf("%s/bot%s/%s", apiBaseURL, a.botToken, method)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("calling Telegram %s: request failed", method)
	}
	defer resp.Body.Close() //nolint:errcheck

	respBody, err := io.ReadAll(io.LimitReader(resp.Body, 10*1024*1024))
	if err != nil {
		return fmt.Errorf("reading response: %w", err)
	}

	var tgResp struct {
		OK          bool   `json:"ok"`
		Description string `json:"description,omitempty"`
	}
	if json.Unmarshal(respBody, &tgResp) == nil && !tgResp.OK {
		return fmt.Errorf("telegram API error: %s", tgResp.Description)
	}

	return nil
}

// splitMessage splits a long message into chunks that fit within maxLen.
func splitMessage(text string, maxLen int) []string {
	if len(text) <= maxLen {
		return []string{text}
	}

	var chunks []string
	for len(text) > 0 {
		if len(text) <= maxLen {
			chunks = append(chunks, text)
			break
		}
		// Try to split at a newline boundary.
		cutAt := maxLen
		if idx := lastIndexByte(text[:maxLen], '\n'); idx > 0 {
			cutAt = idx + 1
		}
		chunks = append(chunks, text[:cutAt])
		text = text[cutAt:]
	}
	return chunks
}

// lastIndexByte returns the last index of byte c in s, or -1.
func lastIndexByte(s string, c byte) int {
	for i := len(s) - 1; i >= 0; i-- {
		if s[i] == c {
			return i
		}
	}
	return -1
}
