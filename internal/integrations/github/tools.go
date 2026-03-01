package github

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

// githubAPIBase is the root URL for the GitHub REST API.
// Exposed as a variable so tests can redirect requests to a local server.
var githubAPIBase = "https://api.github.com"

// ghHTTPClient is used for all outgoing GitHub API requests.
var ghHTTPClient = &http.Client{Timeout: 15 * time.Second}

// ghNoRedirectClient does not follow redirects — used to capture 302 Location headers.
var ghNoRedirectClient = &http.Client{
	Timeout: 15 * time.Second,
	CheckRedirect: func(_ *http.Request, _ []*http.Request) error {
		return http.ErrUseLastResponse
	},
}

// client holds GitHub API credentials and performs authenticated requests.
type client struct {
	token string
}

// call makes a request to the GitHub REST API and returns the raw response body.
func (c *client) call(ctx context.Context, method, path string, body any) (json.RawMessage, error) {
	var reqBody io.Reader
	if body != nil {
		encoded, err := json.Marshal(body)
		if err != nil {
			return nil, fmt.Errorf("marshaling request body: %w", err)
		}
		reqBody = bytes.NewReader(encoded)
	}

	req, err := http.NewRequestWithContext(ctx, method, githubAPIBase+path, reqBody)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	req.Header.Set("Accept", "application/vnd.github.v3+json")
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}

	resp, err := ghHTTPClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("calling GitHub %s %s: request failed", method, path)
	}
	defer resp.Body.Close() //nolint:errcheck

	respBody, err := io.ReadAll(io.LimitReader(resp.Body, 2*1024*1024))
	if err != nil {
		return nil, fmt.Errorf("reading response: %w", err)
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("github API error: status %d: %s", resp.StatusCode, string(respBody))
	}

	return respBody, nil
}

// callRaw makes a request to the GitHub REST API and returns raw bytes (for non-JSON responses).
// The response body is limited to 10MB to accommodate large PR diffs.
func (c *client) callRaw(ctx context.Context, method, path, accept string) ([]byte, error) {
	req, err := http.NewRequestWithContext(ctx, method, githubAPIBase+path, nil)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	req.Header.Set("Accept", accept)

	resp, err := ghHTTPClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("calling GitHub %s %s: request failed", method, path)
	}
	defer resp.Body.Close() //nolint:errcheck

	const maxDiffBytes = 10 * 1024 * 1024 // 10 MB
	respBody, err := io.ReadAll(io.LimitReader(resp.Body, maxDiffBytes))
	if err != nil {
		return nil, fmt.Errorf("reading response: %w", err)
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("github API error: status %d: %s", resp.StatusCode, string(respBody))
	}

	return respBody, nil
}

// getRedirectURL makes a request that expects a redirect and returns the Location URL.
// This is used for endpoints like /actions/runs/{id}/logs that return a 302 to a download URL.
func (c *client) getRedirectURL(ctx context.Context, path string) (string, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, githubAPIBase+path, nil)
	if err != nil {
		return "", fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	req.Header.Set("Accept", "application/vnd.github.v3+json")

	resp, err := ghNoRedirectClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("calling GitHub GET %s: request failed", path)
	}
	defer resp.Body.Close() //nolint:errcheck

	if resp.StatusCode == http.StatusFound || resp.StatusCode == http.StatusMovedPermanently {
		loc := resp.Header.Get("Location")
		if loc == "" {
			return "", fmt.Errorf("redirect response missing Location header")
		}
		return loc, nil
	}

	body, _ := io.ReadAll(io.LimitReader(resp.Body, 512)) //nolint:errcheck
	return "", fmt.Errorf("github API error: status %d: %s", resp.StatusCode, string(body))
}

// splitCSV splits a comma-separated string into a slice of trimmed, non-empty strings.
func splitCSV(s string) []string {
	var result []string
	for _, part := range strings.Split(s, ",") {
		if trimmed := strings.TrimSpace(part); trimmed != "" {
			result = append(result, trimmed)
		}
	}
	return result
}

// textResult is a helper that wraps a string in an MCP CallToolResult.
func textResult(text string) (*mcp.CallToolResult, any, error) {
	return &mcp.CallToolResult{
		Content: []mcp.Content{
			&mcp.TextContent{Text: text},
		},
	}, nil, nil
}
