package github

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

// authHTTPClient is used for credential validation requests.
var authHTTPClient = &http.Client{Timeout: 10 * time.Second}

// userResponse represents the result of the GitHub /user API call.
type userResponse struct {
	Login string `json:"login"`
}

// ValidatePAT calls GitHub's /user API to verify a personal access token.
// On success it returns the GitHub username (login).
func ValidatePAT(ctx context.Context, token string) (string, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, githubAPIBase+"/user", nil)
	if err != nil {
		return "", fmt.Errorf("creating GitHub user request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Accept", "application/vnd.github.v3+json")

	resp, err := authHTTPClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("calling GitHub /user: request failed")
	}
	defer resp.Body.Close() //nolint:errcheck

	body, err := io.ReadAll(io.LimitReader(resp.Body, 2*1024*1024))
	if err != nil {
		return "", fmt.Errorf("reading GitHub response: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("github API error: status %d: %s", resp.StatusCode, string(body))
	}

	var user userResponse
	if err := json.Unmarshal(body, &user); err != nil {
		return "", fmt.Errorf("parsing GitHub user response: %w", err)
	}

	return user.Login, nil
}
