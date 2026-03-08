package telegram

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"unicode/utf8"
)

// RegisterWebhook calls Telegram's setWebhook API to register a webhook URL
// with the given secret token for request verification.
func RegisterWebhook(ctx context.Context, botToken, webhookURL, secretToken string) error {
	payload := map[string]any{
		"url":                  webhookURL,
		"secret_token":         secretToken,
		"allowed_updates":      []string{"message"},
		"drop_pending_updates": false,
	}

	_, err := callTelegram(ctx, botToken, "setWebhook", payload)
	if err != nil {
		return fmt.Errorf("registering webhook: %w", err)
	}
	return nil
}

// DeleteWebhook calls Telegram's deleteWebhook API to remove the webhook.
func DeleteWebhook(ctx context.Context, botToken string) error {
	_, err := callTelegram(ctx, botToken, "deleteWebhook", map[string]any{})
	if err != nil {
		return fmt.Errorf("deleting webhook: %w", err)
	}
	return nil
}

// GenerateSecretToken generates a cryptographically random secret token
// for Telegram webhook verification (64 hex chars = 32 bytes).
func GenerateSecretToken() (string, error) {
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		return "", fmt.Errorf("generating secret token: %w", err)
	}
	return hex.EncodeToString(b), nil
}

// SendChatAction sends a "typing" indicator to a Telegram chat.
func SendChatAction(ctx context.Context, botToken string, chatID int64) {
	payload := map[string]any{
		"chat_id": chatID,
		"action":  "typing",
	}
	_, _ = callTelegram(ctx, botToken, "sendChatAction", payload) //nolint:errcheck
}

// SendReply sends a text message as a reply to a specific message in a Telegram chat.
// If the text exceeds Telegram's 4096 character limit, it is split into multiple messages.
func SendReply(ctx context.Context, botToken string, chatID int64, replyToMsgID int, text string) error {
	const maxLen = 4096

	chunks := splitMessage(text, maxLen)
	for i, chunk := range chunks {
		payload := map[string]any{
			"chat_id": chatID,
			"text":    chunk,
		}
		// Only the first chunk is a reply to the original message.
		if i == 0 && replyToMsgID > 0 {
			payload["reply_to_message_id"] = replyToMsgID
		}

		if _, err := callTelegram(ctx, botToken, "sendMessage", payload); err != nil {
			return fmt.Errorf("sending reply chunk %d: %w", i+1, err)
		}
	}
	return nil
}

// splitMessage splits text into chunks of at most maxLen bytes,
// ensuring splits occur on valid UTF-8 rune boundaries.
func splitMessage(text string, maxLen int) []string {
	if len(text) <= maxLen {
		return []string{text}
	}

	chunks := make([]string, 0, (len(text)/maxLen)+1)
	for len(text) > maxLen {
		end := maxLen
		for end > 0 && !utf8.RuneStart(text[end]) {
			end--
		}
		if end == 0 {
			// Fallback: should not happen with valid UTF-8, but avoid infinite loop.
			end = maxLen
		}
		chunks = append(chunks, text[:end])
		text = text[end:]
	}
	if len(text) > 0 {
		chunks = append(chunks, text)
	}
	return chunks
}
