package github

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

const githubAPIBase = "https://api.github.com"

// ghHTTPClient is used for all outgoing GitHub API requests.
var ghHTTPClient = &http.Client{Timeout: 15 * time.Second}

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

	respBody, err := io.ReadAll(io.LimitReader(resp.Body, 2*1024*1024))
	if err != nil {
		return nil, fmt.Errorf("reading response: %w", err)
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("github API error: status %d: %s", resp.StatusCode, string(respBody))
	}

	return respBody, nil
}

// splitCSV splits a comma-separated string into a slice of trimmed, non-empty strings.
func splitCSV(s string) []string {
	var result []string
	start := 0
	for i := 0; i < len(s); i++ {
		if s[i] == ',' {
			part := trimSpaces(s[start:i])
			if part != "" {
				result = append(result, part)
			}
			start = i + 1
		}
	}
	if part := trimSpaces(s[start:]); part != "" {
		result = append(result, part)
	}
	return result
}

func trimSpaces(s string) string {
	i, j := 0, len(s)
	for i < j && s[i] == ' ' {
		i++
	}
	for j > i && s[j-1] == ' ' {
		j--
	}
	return s[i:j]
}

// textResult is a helper that wraps a string in an MCP CallToolResult.
func textResult(text string) (*mcp.CallToolResult, any, error) {
	return &mcp.CallToolResult{
		Content: []mcp.Content{
			&mcp.TextContent{Text: text},
		},
	}, nil, nil
}
