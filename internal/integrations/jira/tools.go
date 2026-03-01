package jira

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

// client holds Jira API credentials and performs authenticated requests.
type client struct {
	siteURL  string
	email    string
	apiToken string
}

// call makes a request to the Jira REST API and returns the raw response body.
func (c *client) call(ctx context.Context, method, path string, body any) (json.RawMessage, error) {
	var reqBody io.Reader
	if body != nil {
		encoded, err := json.Marshal(body)
		if err != nil {
			return nil, fmt.Errorf("marshaling request body: %w", err)
		}
		reqBody = bytes.NewReader(encoded)
	}

	req, err := http.NewRequestWithContext(ctx, method, c.siteURL+path, reqBody)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}
	req.SetBasicAuth(c.email, c.apiToken)
	req.Header.Set("Accept", "application/json")
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}

	resp, err := jiraHTTPClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("calling Jira %s %s: request failed", method, path)
	}
	defer resp.Body.Close() //nolint:errcheck

	respBody, err := io.ReadAll(io.LimitReader(resp.Body, 10*1024*1024))
	if err != nil {
		return nil, fmt.Errorf("reading response: %w", err)
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("jira API error: status %d: %s", resp.StatusCode, string(respBody))
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

// docBody builds an Atlassian Document Format (ADF) body for plain text.
func docBody(text string) map[string]any {
	return map[string]any{
		"type":    "doc",
		"version": 1,
		"content": []map[string]any{
			{
				"type": "paragraph",
				"content": []map[string]any{
					{"type": "text", "text": text},
				},
			},
		},
	}
}

// registerProjectManagementTools adds all Jira project management MCP tools to the server.
// Only tools whose names are in the allowed set are registered.
// If allowed is empty, all tools are registered.
func registerProjectManagementTools(
	server *mcp.Server, siteURL, email, apiToken string, allowed map[string]bool,
) {
	c := &client{siteURL: siteURL, email: email, apiToken: apiToken}
	registerProjectTools(server, c, allowed)
	registerIssueReadTools(server, c, allowed)
	registerIssueMutationTools(server, c, allowed)
	registerTransitionTools(server, c, allowed)
}

func registerProjectTools(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["list_projects"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "list_projects",
			Description: "Lists all accessible Jira projects.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, _ *struct{}) (*mcp.CallToolResult, any, error) {
			result, err := c.call(ctx, http.MethodGet, "/rest/api/3/project", nil)
			if err != nil {
				return nil, nil, err
			}
			return textResult(fmt.Sprintf("Projects: %s", string(result)))
		})
	}

	if len(allowed) == 0 || allowed["get_project"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "get_project",
			Description: "Gets details of a specific Jira project by key.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *getProjectParams) (*mcp.CallToolResult, any, error) {
			result, err := c.call(ctx, http.MethodGet, "/rest/api/3/project/"+params.Key, nil)
			if err != nil {
				return nil, nil, err
			}
			return textResult(fmt.Sprintf("Project: %s", string(result)))
		})
	}
}

func registerIssueReadTools(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["search_issues"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "search_issues",
			Description: "Searches Jira issues using JQL (Jira Query Language).",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *searchIssuesParams) (*mcp.CallToolResult, any, error) {
			maxResults := params.MaxResults
			if maxResults <= 0 || maxResults > 100 {
				maxResults = 50
			}
			body := map[string]any{"jql": params.JQL, "maxResults": maxResults}
			result, err := c.call(ctx, http.MethodPost, "/rest/api/3/search", body)
			if err != nil {
				return nil, nil, err
			}
			return textResult(fmt.Sprintf("Search results: %s", string(result)))
		})
	}

	if len(allowed) == 0 || allowed["get_issue"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "get_issue",
			Description: "Gets details of a specific Jira issue by key (e.g. PROJ-123).",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *getIssueParams) (*mcp.CallToolResult, any, error) {
			result, err := c.call(ctx, http.MethodGet, "/rest/api/3/issue/"+params.Key, nil)
			if err != nil {
				return nil, nil, err
			}
			return textResult(fmt.Sprintf("Issue: %s", string(result)))
		})
	}
}

func registerIssueMutationTools(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["create_issue"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "create_issue",
			Description: "Creates a new Jira issue in a project.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *createIssueParams) (*mcp.CallToolResult, any, error) {
			return handleCreateIssue(ctx, c, params)
		})
	}

	if len(allowed) == 0 || allowed["update_issue"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "update_issue",
			Description: "Updates fields of an existing Jira issue.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *updateIssueParams) (*mcp.CallToolResult, any, error) {
			return handleUpdateIssue(ctx, c, params)
		})
	}

	if len(allowed) == 0 || allowed["add_comment"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "add_comment",
			Description: "Adds a comment to a Jira issue.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *addCommentParams) (*mcp.CallToolResult, any, error) {
			body := map[string]any{"body": docBody(params.Comment)}
			result, err := c.call(
				ctx, http.MethodPost,
				"/rest/api/3/issue/"+params.Key+"/comment", body,
			)
			if err != nil {
				return nil, nil, err
			}
			return textResult(fmt.Sprintf("Comment added: %s", string(result)))
		})
	}
}

func registerTransitionTools(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["list_transitions"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "list_transitions",
			Description: "Lists available status transitions for a Jira issue.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, p *listTransitionsParams) (*mcp.CallToolResult, any, error) {
			result, err := c.call(ctx, http.MethodGet, "/rest/api/3/issue/"+p.Key+"/transitions", nil)
			if err != nil {
				return nil, nil, err
			}
			return textResult(fmt.Sprintf("Transitions: %s", string(result)))
		})
	}

	if len(allowed) == 0 || allowed["transition_issue"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "transition_issue",
			Description: "Transitions a Jira issue to a new status.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, p *transitionIssueParams) (*mcp.CallToolResult, any, error) {
			body := map[string]any{"transition": map[string]string{"id": p.TransitionID}}
			_, err := c.call(ctx, http.MethodPost, "/rest/api/3/issue/"+p.Key+"/transitions", body)
			if err != nil {
				return nil, nil, err
			}
			return textResult(fmt.Sprintf("Issue %s transitioned successfully.", p.Key))
		})
	}
}

// ── Parameter types and handlers ─────────────────────────────────────────────

type getProjectParams struct {
	Key string `json:"key" jsonschema:"required,The project key (e.g. PROJ)"`
}

type searchIssuesParams struct {
	JQL        string `json:"jql" jsonschema:"required,JQL query string (e.g. project = PROJ AND status = Open)"`
	MaxResults int    `json:"max_results" jsonschema:"Maximum number of issues to return (default 50, max 100)"`
}

type getIssueParams struct {
	Key string `json:"key" jsonschema:"required,The issue key (e.g. PROJ-123)"`
}

type createIssueParams struct {
	ProjectKey  string `json:"project_key" jsonschema:"required,The project key (e.g. PROJ)"`
	IssueType   string `json:"issue_type" jsonschema:"required,Issue type name (e.g. Bug, Story, Task)"`
	Summary     string `json:"summary" jsonschema:"required,Summary/title of the issue"`
	Description string `json:"description" jsonschema:"Optional description of the issue"`
	Priority    string `json:"priority" jsonschema:"Optional priority name (e.g. High, Medium, Low)"`
}

func handleCreateIssue(ctx context.Context, c *client, params *createIssueParams) (*mcp.CallToolResult, any, error) {
	fields := map[string]any{
		"project":   map[string]string{"key": params.ProjectKey},
		"issuetype": map[string]string{"name": params.IssueType},
		"summary":   params.Summary,
	}
	if params.Description != "" {
		fields["description"] = docBody(params.Description)
	}
	if params.Priority != "" {
		fields["priority"] = map[string]string{"name": params.Priority}
	}

	result, err := c.call(ctx, http.MethodPost, "/rest/api/3/issue", map[string]any{"fields": fields})
	if err != nil {
		return nil, nil, err
	}
	return textResult(fmt.Sprintf("Issue created: %s", string(result)))
}

type updateIssueParams struct {
	Key         string `json:"key" jsonschema:"required,The issue key (e.g. PROJ-123)"`
	Summary     string `json:"summary" jsonschema:"Optional new summary/title"`
	Description string `json:"description" jsonschema:"Optional new description"`
	Priority    string `json:"priority" jsonschema:"Optional new priority name (e.g. High, Medium, Low)"`
}

func handleUpdateIssue(ctx context.Context, c *client, params *updateIssueParams) (*mcp.CallToolResult, any, error) {
	fields := map[string]any{}
	if params.Summary != "" {
		fields["summary"] = params.Summary
	}
	if params.Description != "" {
		fields["description"] = docBody(params.Description)
	}
	if params.Priority != "" {
		fields["priority"] = map[string]string{"name": params.Priority}
	}

	_, err := c.call(
		ctx, http.MethodPut,
		"/rest/api/3/issue/"+params.Key, map[string]any{"fields": fields},
	)
	if err != nil {
		return nil, nil, err
	}
	return textResult(fmt.Sprintf("Issue %s updated successfully.", params.Key))
}

type addCommentParams struct {
	Key     string `json:"key" jsonschema:"required,The issue key (e.g. PROJ-123)"`
	Comment string `json:"comment" jsonschema:"required,The text of the comment to add"`
}

type listTransitionsParams struct {
	Key string `json:"key" jsonschema:"required,The issue key (e.g. PROJ-123)"`
}

type transitionIssueParams struct {
	Key          string `json:"key" jsonschema:"required,The issue key (e.g. PROJ-123)"`
	TransitionID string `json:"transition_id" jsonschema:"required,The ID of the transition to perform"`
}
