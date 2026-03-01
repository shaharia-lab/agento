package github

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"strconv"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

// registerIssuesTools adds GitHub issue MCP tools to the server.
func registerIssuesTools(server *mcp.Server, token string, allowed map[string]bool) {
	c := &client{token: token}
	registerListIssues(server, c, allowed)
	registerGetIssue(server, c, allowed)
	registerCreateIssue(server, c, allowed)
	registerUpdateIssue(server, c, allowed)
}

func registerListIssues(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["list_issues"] {
		return
	}
	type params struct {
		Owner   string `json:"owner" jsonschema:"required,Repository owner"`
		Repo    string `json:"repo" jsonschema:"required,Repository name"`
		State   string `json:"state" jsonschema:"Filter: open, closed, all. Default: open"`
		Labels  string `json:"labels" jsonschema:"Comma-separated label names"`
		Sort    string `json:"sort" jsonschema:"Sort: created, updated, comments"`
		PerPage int    `json:"per_page" jsonschema:"Results per page (max 100)"`
		Page    int    `json:"page" jsonschema:"Page number. Default: 1"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "list_issues",
		Description: "Lists issues for a repository.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		q := url.Values{}
		if p.State != "" {
			q.Set("state", p.State)
		}
		if p.Labels != "" {
			q.Set("labels", p.Labels)
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
		path := fmt.Sprintf("/repos/%s/%s/issues?%s",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo), q.Encode())
		result, err := c.call(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Issues: %s", string(result)))
	})
}

func registerGetIssue(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["get_issue"] {
		return
	}
	type params struct {
		Owner  string `json:"owner" jsonschema:"required,Repository owner"`
		Repo   string `json:"repo" jsonschema:"required,Repository name"`
		Number int    `json:"number" jsonschema:"required,Issue number"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "get_issue",
		Description: "Gets details of a specific issue by number.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		path := fmt.Sprintf("/repos/%s/%s/issues/%d",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo), p.Number)
		result, err := c.call(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Issue: %s", string(result)))
	})
}

func registerCreateIssue(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["create_issue"] {
		return
	}
	type params struct {
		Owner     string `json:"owner" jsonschema:"required,Repository owner"`
		Repo      string `json:"repo" jsonschema:"required,Repository name"`
		Title     string `json:"title" jsonschema:"required,Issue title"`
		Body      string `json:"body" jsonschema:"Issue body in Markdown"`
		Labels    string `json:"labels" jsonschema:"Comma-separated label names"`
		Assignees string `json:"assignees" jsonschema:"Comma-separated usernames"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "create_issue",
		Description: "Creates a new issue in a repository.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		body := map[string]any{"title": p.Title}
		if p.Body != "" {
			body["body"] = p.Body
		}
		if p.Labels != "" {
			body["labels"] = splitCSV(p.Labels)
		}
		if p.Assignees != "" {
			body["assignees"] = splitCSV(p.Assignees)
		}
		path := fmt.Sprintf("/repos/%s/%s/issues",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo))
		result, err := c.call(ctx, http.MethodPost, path, body)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Issue created: %s", string(result)))
	})
}

func registerUpdateIssue(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["update_issue"] {
		return
	}
	type params struct {
		Owner  string `json:"owner" jsonschema:"required,Repository owner"`
		Repo   string `json:"repo" jsonschema:"required,Repository name"`
		Number int    `json:"number" jsonschema:"required,Issue number"`
		Title  string `json:"title" jsonschema:"New title"`
		Body   string `json:"body" jsonschema:"New body in Markdown"`
		State  string `json:"state" jsonschema:"New state: open or closed"`
		Labels string `json:"labels" jsonschema:"Comma-separated label names"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "update_issue",
		Description: "Updates an existing issue.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		body := map[string]any{}
		if p.Title != "" {
			body["title"] = p.Title
		}
		if p.Body != "" {
			body["body"] = p.Body
		}
		if p.State != "" {
			body["state"] = p.State
		}
		if p.Labels != "" {
			body["labels"] = splitCSV(p.Labels)
		}
		path := fmt.Sprintf("/repos/%s/%s/issues/%d",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo), p.Number)
		result, err := c.call(ctx, http.MethodPatch, path, body)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Issue updated: %s", string(result)))
	})
}
