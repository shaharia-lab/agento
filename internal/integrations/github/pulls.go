package github

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"strconv"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

// registerPullsTools adds GitHub pull request MCP tools to the server.
func registerPullsTools(server *mcp.Server, token string, allowed map[string]bool) {
	c := &client{token: token}
	registerListPulls(server, c, allowed)
	registerGetPull(server, c, allowed)
	registerCreatePull(server, c, allowed)
	registerGetPullDiff(server, c, allowed)
	registerListPullComments(server, c, allowed)
}

func registerListPulls(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["list_pulls"] {
		return
	}
	type params struct {
		Owner   string `json:"owner" jsonschema:"required,Repository owner"`
		Repo    string `json:"repo" jsonschema:"required,Repository name"`
		State   string `json:"state" jsonschema:"Filter: open, closed, all"`
		Sort    string `json:"sort" jsonschema:"Sort: created, updated, popularity"`
		Base    string `json:"base" jsonschema:"Filter by base branch name"`
		PerPage int    `json:"per_page" jsonschema:"Results per page (max 100)"`
		Page    int    `json:"page" jsonschema:"Page number. Default: 1"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "list_pulls",
		Description: "Lists pull requests for a repository.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		q := url.Values{}
		if p.State != "" {
			q.Set("state", p.State)
		}
		if p.Sort != "" {
			q.Set("sort", p.Sort)
		}
		if p.Base != "" {
			q.Set("base", p.Base)
		}
		perPage := p.PerPage
		if perPage <= 0 || perPage > 100 {
			perPage = 30
		}
		q.Set("per_page", strconv.Itoa(perPage))
		if p.Page > 0 {
			q.Set("page", strconv.Itoa(p.Page))
		}
		path := fmt.Sprintf("/repos/%s/%s/pulls?%s",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo), q.Encode())
		result, err := c.call(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Pull requests: %s", string(result)))
	})
}

func registerGetPull(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["get_pull"] {
		return
	}
	type params struct {
		Owner  string `json:"owner" jsonschema:"required,Repository owner"`
		Repo   string `json:"repo" jsonschema:"required,Repository name"`
		Number int    `json:"number" jsonschema:"required,Pull request number"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "get_pull",
		Description: "Gets details of a specific pull request by number.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		path := fmt.Sprintf("/repos/%s/%s/pulls/%d",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo), p.Number)
		result, err := c.call(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Pull request: %s", string(result)))
	})
}

func registerCreatePull(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["create_pull"] {
		return
	}
	type params struct {
		Owner string `json:"owner" jsonschema:"required,Repository owner"`
		Repo  string `json:"repo" jsonschema:"required,Repository name"`
		Title string `json:"title" jsonschema:"required,Pull request title"`
		Head  string `json:"head" jsonschema:"required,Source branch name"`
		Base  string `json:"base" jsonschema:"required,Target branch name"`
		Body  string `json:"body" jsonschema:"PR body in Markdown"`
		Draft bool   `json:"draft" jsonschema:"Create as draft. Default: false"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "create_pull",
		Description: "Creates a new pull request.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		body := map[string]any{
			"title": p.Title,
			"head":  p.Head,
			"base":  p.Base,
		}
		if p.Body != "" {
			body["body"] = p.Body
		}
		if p.Draft {
			body["draft"] = true
		}
		path := fmt.Sprintf("/repos/%s/%s/pulls",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo))
		result, err := c.call(ctx, http.MethodPost, path, body)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Pull request created: %s", string(result)))
	})
}

func registerGetPullDiff(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["get_pull_diff"] {
		return
	}
	type params struct {
		Owner  string `json:"owner" jsonschema:"required,Repository owner"`
		Repo   string `json:"repo" jsonschema:"required,Repository name"`
		Number int    `json:"number" jsonschema:"required,Pull request number"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "get_pull_diff",
		Description: "Gets the diff of a pull request.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		path := fmt.Sprintf("/repos/%s/%s/pulls/%d",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo), p.Number)
		result, err := c.callRaw(
			ctx, http.MethodGet, path, "application/vnd.github.v3.diff",
		)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Diff:\n%s", string(result)))
	})
}

func registerListPullComments(
	server *mcp.Server, c *client, allowed map[string]bool,
) {
	if len(allowed) > 0 && !allowed["list_pull_comments"] {
		return
	}
	type params struct {
		Owner   string `json:"owner" jsonschema:"required,Repository owner"`
		Repo    string `json:"repo" jsonschema:"required,Repository name"`
		Number  int    `json:"number" jsonschema:"required,Pull request number"`
		PerPage int    `json:"per_page" jsonschema:"Results per page (max 100)"`
		Page    int    `json:"page" jsonschema:"Page number. Default: 1"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "list_pull_comments",
		Description: "Lists review comments on a pull request.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		q := url.Values{}
		perPage := p.PerPage
		if perPage <= 0 || perPage > 100 {
			perPage = 30
		}
		q.Set("per_page", strconv.Itoa(perPage))
		if p.Page > 0 {
			q.Set("page", strconv.Itoa(p.Page))
		}
		path := fmt.Sprintf("/repos/%s/%s/pulls/%d/comments?%s",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo),
			p.Number, q.Encode())
		result, err := c.call(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Comments: %s", string(result)))
	})
}
