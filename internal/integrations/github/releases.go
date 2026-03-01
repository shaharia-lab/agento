package github

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"strconv"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

// registerReleasesTools adds GitHub release MCP tools to the server.
func registerReleasesTools(server *mcp.Server, token string, allowed map[string]bool) {
	c := &client{token: token}
	registerListReleases(server, c, allowed)
	registerCreateRelease(server, c, allowed)
	registerListTags(server, c, allowed)
}

func registerListReleases(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["list_releases"] {
		return
	}
	type params struct {
		Owner   string `json:"owner" jsonschema:"required,Repository owner"`
		Repo    string `json:"repo" jsonschema:"required,Repository name"`
		PerPage int    `json:"per_page" jsonschema:"Results per page (max 100)"`
		Page    int    `json:"page" jsonschema:"Page number. Default: 1"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "list_releases",
		Description: "Lists releases for a repository.",
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
		path := fmt.Sprintf("/repos/%s/%s/releases?%s",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo), q.Encode())
		result, err := c.call(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Releases: %s", string(result)))
	})
}

type createReleaseParams struct {
	Owner           string `json:"owner" jsonschema:"required,Repository owner"`
	Repo            string `json:"repo" jsonschema:"required,Repository name"`
	TagName         string `json:"tag_name" jsonschema:"required,Tag name"`
	Name            string `json:"name" jsonschema:"Release name"`
	Body            string `json:"body" jsonschema:"Release notes in Markdown"`
	TargetCommitish string `json:"target_commitish" jsonschema:"Branch or commit SHA"`
	Draft           bool   `json:"draft" jsonschema:"Create as draft"`
	Prerelease      bool   `json:"prerelease" jsonschema:"Mark as pre-release"`
	GenerateNotes   bool   `json:"generate_release_notes" jsonschema:"Auto-generate notes"`
}

func buildReleaseBody(p *createReleaseParams) map[string]any {
	body := map[string]any{"tag_name": p.TagName}
	if p.Name != "" {
		body["name"] = p.Name
	}
	if p.Body != "" {
		body["body"] = p.Body
	}
	if p.TargetCommitish != "" {
		body["target_commitish"] = p.TargetCommitish
	}
	if p.Draft {
		body["draft"] = true
	}
	if p.Prerelease {
		body["prerelease"] = true
	}
	if p.GenerateNotes {
		body["generate_release_notes"] = true
	}
	return body
}

func registerCreateRelease(
	server *mcp.Server, c *client, allowed map[string]bool,
) {
	if len(allowed) > 0 && !allowed["create_release"] {
		return
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "create_release",
		Description: "Creates a new release in a repository.",
	}, func(
		ctx context.Context,
		_ *mcp.CallToolRequest,
		p *createReleaseParams,
	) (*mcp.CallToolResult, any, error) {
		body := buildReleaseBody(p)
		path := fmt.Sprintf("/repos/%s/%s/releases",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo))
		result, err := c.call(ctx, http.MethodPost, path, body)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Release created: %s", string(result)))
	})
}

func registerListTags(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["list_tags"] {
		return
	}
	type params struct {
		Owner   string `json:"owner" jsonschema:"required,Repository owner"`
		Repo    string `json:"repo" jsonschema:"required,Repository name"`
		PerPage int    `json:"per_page" jsonschema:"Results per page (max 100)"`
		Page    int    `json:"page" jsonschema:"Page number. Default: 1"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "list_tags",
		Description: "Lists tags for a repository.",
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
		path := fmt.Sprintf("/repos/%s/%s/tags?%s",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo), q.Encode())
		result, err := c.call(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Tags: %s", string(result)))
	})
}
