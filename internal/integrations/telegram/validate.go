package telegram

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

// telegramHTTPClient is used for all outgoing Telegram API requests.
var telegramHTTPClient = &http.Client{Timeout: 60 * time.Second}

// telegramResponse wraps the standard Telegram Bot API response envelope.
type telegramResponse struct {
	OK          bool            `json:"ok"`
	Description string          `json:"description,omitempty"`
	Result      json.RawMessage `json:"result,omitempty"`
}

// botUser represents the result of the getMe API call.
type botUser struct {
	ID        int64  `json:"id"`
	IsBot     bool   `json:"is_bot"`
	FirstName string `json:"first_name"`
	Username  string `json:"username"`
}

// ValidateBotToken calls the Telegram getMe API to verify a bot token is valid.
// On success it returns the bot's username.
func ValidateBotToken(ctx context.Context, token string) (string, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, apiURL(token, "getMe"), nil)
	if err != nil {
		return "", fmt.Errorf("creating Telegram getMe request: %w", err)
	}

	resp, err := telegramHTTPClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("calling Telegram getMe: request failed")
	}
	defer resp.Body.Close() //nolint:errcheck

	body, err := io.ReadAll(io.LimitReader(resp.Body, 10*1024*1024))
	if err != nil {
		return "", fmt.Errorf("reading Telegram response: %w", err)
	}

	var tgResp telegramResponse
	if err := json.Unmarshal(body, &tgResp); err != nil {
		return "", fmt.Errorf("parsing Telegram response: %w", err)
	}

	if !tgResp.OK {
		return "", fmt.Errorf("telegram API error: %s", tgResp.Description)
	}

	var bot botUser
	if err := json.Unmarshal(tgResp.Result, &bot); err != nil {
		return "", fmt.Errorf("parsing bot user: %w", err)
	}

	return bot.Username, nil
}
