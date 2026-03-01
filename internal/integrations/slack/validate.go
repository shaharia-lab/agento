package slack

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

// slackHTTPClient is used for all outgoing Slack API requests.
var slackHTTPClient = &http.Client{Timeout: 60 * time.Second}

// slackAPIBase is the base URL for the Slack Web API.
// It is a variable so tests can point it at a local httptest server.
var slackAPIBase = "https://slack.com/api"

// authTestResponse holds the result of the auth.test API call.
type authTestResponse struct {
	OK     bool   `json:"ok"`
	Error  string `json:"error,omitempty"`
	URL    string `json:"url,omitempty"`
	Team   string `json:"team,omitempty"`
	User   string `json:"user,omitempty"`
	TeamID string `json:"team_id,omitempty"`
	UserID string `json:"user_id,omitempty"`
	BotID  string `json:"bot_id,omitempty"`
}

// ValidateToken calls the Slack auth.test API to verify a token is valid.
// On success it returns the team name.
func ValidateToken(ctx context.Context, token string) (string, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, slackAPIBase+"/auth.test", nil)
	if err != nil {
		return "", fmt.Errorf("creating Slack auth.test request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json")

	resp, err := slackHTTPClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("calling Slack auth.test: request failed")
	}
	defer resp.Body.Close() //nolint:errcheck

	if resp.StatusCode == http.StatusTooManyRequests {
		retryAfter := resp.Header.Get("Retry-After")
		return "", fmt.Errorf("slack rate limited, retry after %s seconds", retryAfter)
	}

	body, err := io.ReadAll(io.LimitReader(resp.Body, 5*1024*1024))
	if err != nil {
		return "", fmt.Errorf("reading Slack response: %w", err)
	}

	var result authTestResponse
	if err := json.Unmarshal(body, &result); err != nil {
		return "", fmt.Errorf("parsing Slack response: %w", err)
	}

	if !result.OK {
		return "", fmt.Errorf("slack API error: %s", result.Error)
	}

	return result.Team, nil
}
