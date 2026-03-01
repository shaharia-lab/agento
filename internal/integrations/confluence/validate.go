package confluence

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

// confluenceHTTPClient is used for all outgoing Confluence API requests.
var confluenceHTTPClient = &http.Client{Timeout: 30 * time.Second}

// confluenceSpacesResponse wraps the Confluence v2 spaces list response.
type confluenceSpacesResponse struct {
	Results []struct {
		ID   string `json:"id"`
		Key  string `json:"key"`
		Name string `json:"name"`
	} `json:"results"`
}

// ValidateCredentials calls the Confluence API to verify credentials are valid.
// On success it returns the site URL (trimmed).
func ValidateCredentials(ctx context.Context, siteURL, email, apiToken string) error {
	url := fmt.Sprintf("%s/wiki/api/v2/spaces?limit=1", siteURL)

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return fmt.Errorf("creating Confluence request: %w", err)
	}
	req.SetBasicAuth(email, apiToken)
	req.Header.Set("Accept", "application/json")

	resp, err := confluenceHTTPClient.Do(req)
	if err != nil {
		return fmt.Errorf("calling Confluence API: request failed")
	}
	defer resp.Body.Close() //nolint:errcheck

	body, err := io.ReadAll(io.LimitReader(resp.Body, 1*1024*1024))
	if err != nil {
		return fmt.Errorf("reading Confluence response: %w", err)
	}

	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return fmt.Errorf("invalid credentials: check email and API token")
	}

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("confluence API returned status %d: %s", resp.StatusCode, string(body))
	}

	var spacesResp confluenceSpacesResponse
	if err := json.Unmarshal(body, &spacesResp); err != nil {
		return fmt.Errorf("parsing Confluence response: %w", err)
	}

	return nil
}
