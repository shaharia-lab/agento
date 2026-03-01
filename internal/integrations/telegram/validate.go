package telegram

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
)

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
func ValidateBotToken(token string) (string, error) {
	url := fmt.Sprintf("https://api.telegram.org/bot%s/getMe", token)

	resp, err := http.Get(url) //nolint:gosec,noctx // validation-only call with user-provided token
	if err != nil {
		return "", fmt.Errorf("calling Telegram getMe: %w", err)
	}
	defer resp.Body.Close() //nolint:errcheck

	body, err := io.ReadAll(resp.Body)
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
