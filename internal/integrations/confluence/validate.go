package confluence

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
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

// ValidateSiteURL checks that siteURL is a valid HTTPS URL.
// It also strips any trailing slash so callers can concatenate paths directly.
func ValidateSiteURL(rawURL string) (string, error) {
	u, err := url.Parse(rawURL)
	if err != nil {
		return "", fmt.Errorf("invalid site URL: %w", err)
	}
	if u.Scheme != "https" {
		return "", fmt.Errorf("site URL must use HTTPS (got %q)", u.Scheme)
	}
	if u.Host == "" {
		return "", fmt.Errorf("site URL must include a hostname")
	}
	return strings.TrimRight(rawURL, "/"), nil
}

// ValidateCredentials calls the Confluence API to verify credentials are valid.
func ValidateCredentials(ctx context.Context, siteURL, email, apiToken string) error {
	cleanURL, err := ValidateSiteURL(siteURL)
	if err != nil {
		return err
	}

	reqURL := cleanURL + "/wiki/api/v2/spaces?limit=1"

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, reqURL, nil)
	if err != nil {
		return fmt.Errorf("creating confluence request: %w", err)
	}
	req.SetBasicAuth(email, apiToken)
	req.Header.Set("Accept", "application/json")

	resp, err := confluenceHTTPClient.Do(req)
	if err != nil {
		return fmt.Errorf("calling confluence API: request failed")
	}
	defer resp.Body.Close() //nolint:errcheck

	body, err := io.ReadAll(io.LimitReader(resp.Body, 1*1024*1024))
	if err != nil {
		return fmt.Errorf("reading confluence response: %w", err)
	}

	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return fmt.Errorf("invalid credentials: check email and API token")
	}

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("confluence API returned status %d: %s", resp.StatusCode, string(body))
	}

	var spacesResp confluenceSpacesResponse
	if err := json.Unmarshal(body, &spacesResp); err != nil {
		return fmt.Errorf("parsing confluence response: %w", err)
	}

	return nil
}
