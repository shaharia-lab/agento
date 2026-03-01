package confluence

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

// maxResponseBytes is the maximum response body size read from the Confluence API.
// Keeping it small avoids flooding the LLM context with large payloads.
const maxResponseBytes = 2 * 1024 * 1024 // 2 MB

// callConfluence makes an authenticated request to the Confluence API and returns the body bytes.
func callConfluence(ctx context.Context, method, reqURL, email, apiToken string, body io.Reader) ([]byte, error) {
	req, err := http.NewRequestWithContext(ctx, method, reqURL, body)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}
	req.SetBasicAuth(email, apiToken)
	req.Header.Set("Accept", "application/json")
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}

	resp, err := confluenceHTTPClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("calling confluence API: request failed")
	}
	defer resp.Body.Close() //nolint:errcheck

	respBody, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseBytes))
	if err != nil {
		return nil, fmt.Errorf("reading response: %w", err)
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("confluence API error (status %d): %s", resp.StatusCode, string(respBody))
	}

	return respBody, nil
}

// textResult is a helper that wraps a string in an MCP CallToolResult.
func textResult(text string) (*mcp.CallToolResult, any, error) {
	return &mcp.CallToolResult{
		Content: []mcp.Content{
			&mcp.TextContent{Text: text},
		},
	}, nil, nil
}

// registerContentTools adds all Confluence content MCP tools to the server.
// Only tools whose names are in the allowed set are registered.
// If allowed is empty, all tools are registered.
func registerContentTools(server *mcp.Server, siteURL, email, apiToken string, allowed map[string]bool) {
	registerSpaceTools(server, siteURL, email, apiToken, allowed)
	registerPageTools(server, siteURL, email, apiToken, allowed)
}

func registerSpaceTools(server *mcp.Server, siteURL, email, apiToken string, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["list_spaces"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "list_spaces",
			Description: "Lists Confluence spaces.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *listSpacesParams) (*mcp.CallToolResult, any, error) {
			return handleListSpaces(ctx, siteURL, email, apiToken, params)
		})
	}

	if len(allowed) == 0 || allowed["get_space"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "get_space",
			Description: "Gets details of a Confluence space by ID.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *getSpaceParams) (*mcp.CallToolResult, any, error) {
			return handleGetSpace(ctx, siteURL, email, apiToken, params)
		})
	}

	if len(allowed) == 0 || allowed["search_content"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "search_content",
			Description: "Searches Confluence content using CQL (Confluence Query Language).",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *searchContentParams) (*mcp.CallToolResult, any, error) {
			return handleSearchContent(ctx, siteURL, email, apiToken, params)
		})
	}
}

func registerPageTools(server *mcp.Server, siteURL, email, apiToken string, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["get_page"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "get_page",
			Description: "Gets the content and metadata of a Confluence page by ID.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *getPageParams) (*mcp.CallToolResult, any, error) {
			return handleGetPage(ctx, siteURL, email, apiToken, params)
		})
	}

	if len(allowed) == 0 || allowed["create_page"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "create_page",
			Description: "Creates a new Confluence page in a given space.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *createPageParams) (*mcp.CallToolResult, any, error) {
			return handleCreatePage(ctx, siteURL, email, apiToken, params)
		})
	}

	if len(allowed) == 0 || allowed["update_page"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "update_page",
			Description: "Updates an existing Confluence page.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *updatePageParams) (*mcp.CallToolResult, any, error) {
			return handleUpdatePage(ctx, siteURL, email, apiToken, params)
		})
	}
}

// ── Parameter types and handlers ─────────────────────────────────────────────

// list_spaces

type listSpacesParams struct {
	Limit int `json:"limit" jsonschema:"Maximum number of spaces to return (1-250)"`
}

func handleListSpaces(
	ctx context.Context, siteURL, email, apiToken string, params *listSpacesParams,
) (*mcp.CallToolResult, any, error) {
	limit := params.Limit
	if limit <= 0 || limit > 250 {
		limit = 50
	}
	reqURL := fmt.Sprintf("%s/wiki/api/v2/spaces?limit=%d", siteURL, limit)

	body, err := callConfluence(ctx, http.MethodGet, reqURL, email, apiToken, nil)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Spaces: %s", string(body)))
}

// get_space

type getSpaceParams struct {
	SpaceID string `json:"space_id" jsonschema:"required,The ID of the space to retrieve"`
}

func handleGetSpace(
	ctx context.Context, siteURL, email, apiToken string, params *getSpaceParams,
) (*mcp.CallToolResult, any, error) {
	reqURL := fmt.Sprintf("%s/wiki/api/v2/spaces/%s", siteURL, url.PathEscape(params.SpaceID))

	body, err := callConfluence(ctx, http.MethodGet, reqURL, email, apiToken, nil)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Space: %s", string(body)))
}

// search_content

type searchContentParams struct {
	CQL   string `json:"cql" jsonschema:"required,CQL query string (e.g. 'space = DEV AND type = page')"`
	Limit int    `json:"limit" jsonschema:"Maximum number of results to return (1-250)"`
}

func handleSearchContent(
	ctx context.Context, siteURL, email, apiToken string, params *searchContentParams,
) (*mcp.CallToolResult, any, error) {
	limit := params.Limit
	if limit <= 0 || limit > 250 {
		limit = 25
	}
	reqURL := fmt.Sprintf("%s/wiki/api/v2/search?cql=%s&limit=%d",
		siteURL, url.QueryEscape(params.CQL), limit)

	body, err := callConfluence(ctx, http.MethodGet, reqURL, email, apiToken, nil)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Search results: %s", string(body)))
}

// get_page

type getPageParams struct {
	PageID     string `json:"page_id" jsonschema:"required,The ID of the page to retrieve"`
	BodyFormat string `json:"body_format" jsonschema:"Body format to return: storage or view (default: storage)"`
}

func handleGetPage(
	ctx context.Context, siteURL, email, apiToken string, params *getPageParams,
) (*mcp.CallToolResult, any, error) {
	format := params.BodyFormat
	if format == "" {
		format = "storage"
	}
	reqURL := fmt.Sprintf("%s/wiki/api/v2/pages/%s?body-format=%s",
		siteURL, url.PathEscape(params.PageID), url.QueryEscape(format))

	body, err := callConfluence(ctx, http.MethodGet, reqURL, email, apiToken, nil)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Page: %s", string(body)))
}

// create_page

type createPageParams struct {
	SpaceID  string `json:"space_id" jsonschema:"required,The ID of the space to create the page in"`
	Title    string `json:"title" jsonschema:"required,Title of the new page"`
	Body     string `json:"body" jsonschema:"required,Page body content in Confluence storage format (XHTML)"`
	ParentID string `json:"parent_id" jsonschema:"Optional parent page ID to nest under"`
}

func handleCreatePage(
	ctx context.Context, siteURL, email, apiToken string, params *createPageParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"spaceId": params.SpaceID,
		"status":  "current",
		"title":   params.Title,
		"body": map[string]any{
			"representation": "storage",
			"value":          params.Body,
		},
	}
	if params.ParentID != "" {
		payload["parentId"] = params.ParentID
	}

	payloadBytes, err := json.Marshal(payload)
	if err != nil {
		return nil, nil, fmt.Errorf("marshaling request: %w", err)
	}

	reqURL := siteURL + "/wiki/api/v2/pages"
	respBody, err := callConfluence(ctx, http.MethodPost, reqURL, email, apiToken, strings.NewReader(string(payloadBytes)))
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Page created: %s", string(respBody)))
}

// update_page

type updatePageParams struct {
	PageID  string `json:"page_id" jsonschema:"required,The ID of the page to update"`
	Title   string `json:"title" jsonschema:"required,New title for the page"`
	Body    string `json:"body" jsonschema:"required,New page body content in Confluence storage format (XHTML)"`
	Version int    `json:"version" jsonschema:"required,Current version number of the page (incremented by 1 on update)"`
}

func handleUpdatePage(
	ctx context.Context, siteURL, email, apiToken string, params *updatePageParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"id":     params.PageID,
		"status": "current",
		"title":  params.Title,
		"body": map[string]any{
			"representation": "storage",
			"value":          params.Body,
		},
		"version": map[string]any{
			"number": params.Version + 1,
		},
	}

	payloadBytes, err := json.Marshal(payload)
	if err != nil {
		return nil, nil, fmt.Errorf("marshaling request: %w", err)
	}

	reqURL := fmt.Sprintf("%s/wiki/api/v2/pages/%s", siteURL, url.PathEscape(params.PageID))
	respBody, err := callConfluence(ctx, http.MethodPut, reqURL, email, apiToken, strings.NewReader(string(payloadBytes)))
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Page updated: %s", string(respBody)))
}
