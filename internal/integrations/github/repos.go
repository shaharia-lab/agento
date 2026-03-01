package github

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"strconv"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

// registerReposTools adds GitHub repository MCP tools to the server.
func registerReposTools(server *mcp.Server, token string, allowed map[string]bool) {
	c := &client{token: token}
	registerListRepos(server, c, allowed)
	registerGetRepo(server, c, allowed)
	registerSearchCode(server, c, allowed)
}

func registerListRepos(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["list_repos"] {
		return
	}
	type params struct {
		Visibility string `json:"visibility" jsonschema:"Filter by visibility: all, public, private"`
		Sort       string `json:"sort" jsonschema:"Sort by: created, updated, pushed, full_name"`
		PerPage    int    `json:"per_page" jsonschema:"Results per page (max 100). Default: 30"`
		Page       int    `json:"page" jsonschema:"Page number. Default: 1"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "list_repos",
		Description: "Lists repositories for the authenticated user.",
	}, func(ctx context.Context, _ *mcp.CallToolRequest, p *params) (*mcp.CallToolResult, any, error) {
		q := url.Values{}
		if p.Visibility != "" {
			q.Set("visibility", p.Visibility)
		}
		if p.Sort != "" {
			q.Set("sort", p.Sort)
		}
		perPage := p.PerPage
		if perPage <= 0 || perPage > 100 {
			perPage = 30
		}
		q.Set("per_page", strconv.Itoa(perPage))
		if p.Page > 0 {
			q.Set("page", strconv.Itoa(p.Page))
		}
		result, err := c.call(ctx, http.MethodGet, "/user/repos?"+q.Encode(), nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Repositories: %s", string(result)))
	})
}

func registerGetRepo(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["get_repo"] {
		return
	}
	type params struct {
		Owner string `json:"owner" jsonschema:"required,Repository owner"`
		Repo  string `json:"repo" jsonschema:"required,Repository name"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "get_repo",
		Description: "Gets details of a specific repository by owner/name.",
	}, func(ctx context.Context, _ *mcp.CallToolRequest, p *params) (*mcp.CallToolResult, any, error) {
		path := fmt.Sprintf("/repos/%s/%s",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo))
		result, err := c.call(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Repository: %s", string(result)))
	})
}

func registerSearchCode(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["search_code"] {
		return
	}
	type params struct {
		Query   string `json:"query" jsonschema:"required,Search query"`
		PerPage int    `json:"per_page" jsonschema:"Results per page (max 100). Default: 30"`
		Page    int    `json:"page" jsonschema:"Page number. Default: 1"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "search_code",
		Description: "Searches for code across GitHub repositories using a query string.",
	}, func(ctx context.Context, _ *mcp.CallToolRequest, p *params) (*mcp.CallToolResult, any, error) {
		q := url.Values{}
		q.Set("q", p.Query)
		perPage := p.PerPage
		if perPage <= 0 || perPage > 100 {
			perPage = 30
		}
		q.Set("per_page", strconv.Itoa(perPage))
		if p.Page > 0 {
			q.Set("page", strconv.Itoa(p.Page))
		}
		result, err := c.call(ctx, http.MethodGet, "/search/code?"+q.Encode(), nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Search results: %s", string(result)))
	})
}
