package jira

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

// jiraHTTPClient is used for all outgoing Jira API requests.
var jiraHTTPClient = &http.Client{Timeout: 15 * time.Second}

// myselfResponse represents the result of the /rest/api/3/myself API call.
type myselfResponse struct {
	DisplayName  string `json:"displayName"`
	EmailAddress string `json:"emailAddress"`
}

// ValidateCredentials calls Jira's /rest/api/3/myself API to verify credentials.
// On success it returns the Jira user's display name.
func ValidateCredentials(ctx context.Context, siteURL, email, apiToken string) (string, error) {
	url := siteURL + "/rest/api/3/myself"
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return "", fmt.Errorf("creating Jira myself request: %w", err)
	}
	req.SetBasicAuth(email, apiToken)
	req.Header.Set("Accept", "application/json")

	resp, err := jiraHTTPClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("calling Jira /myself: request failed")
	}
	defer resp.Body.Close() //nolint:errcheck

	body, err := io.ReadAll(io.LimitReader(resp.Body, 10*1024*1024))
	if err != nil {
		return "", fmt.Errorf("reading Jira response: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("jira API error: status %d: %s", resp.StatusCode, string(body))
	}

	var myself myselfResponse
	if err := json.Unmarshal(body, &myself); err != nil {
		return "", fmt.Errorf("parsing Jira myself response: %w", err)
	}

	return myself.DisplayName, nil
}
