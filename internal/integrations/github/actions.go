package github

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"strconv"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

// registerActionsTools adds GitHub Actions MCP tools to the server.
func registerActionsTools(server *mcp.Server, token string, allowed map[string]bool) {
	c := &client{token: token}
	registerListWorkflows(server, c, allowed)
	registerListWorkflowRuns(server, c, allowed)
	registerTriggerWorkflow(server, c, allowed)
	registerGetWorkflowRun(server, c, allowed)
	registerGetRunLogs(server, c, allowed)
}

func registerListWorkflows(server *mcp.Server, c *client, allowed map[string]bool) {
	if len(allowed) > 0 && !allowed["list_workflows"] {
		return
	}
	type params struct {
		Owner   string `json:"owner" jsonschema:"required,Repository owner"`
		Repo    string `json:"repo" jsonschema:"required,Repository name"`
		PerPage int    `json:"per_page" jsonschema:"Results per page (max 100)"`
		Page    int    `json:"page" jsonschema:"Page number. Default: 1"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "list_workflows",
		Description: "Lists all workflows in a repository.",
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
		path := fmt.Sprintf("/repos/%s/%s/actions/workflows?%s",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo), q.Encode())
		result, err := c.call(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Workflows: %s", string(result)))
	})
}

type listWorkflowRunsParams struct {
	Owner      string `json:"owner" jsonschema:"required,Repository owner"`
	Repo       string `json:"repo" jsonschema:"required,Repository name"`
	WorkflowID string `json:"workflow_id" jsonschema:"Workflow ID or file name"`
	Status     string `json:"status" jsonschema:"Filter: queued, in_progress, completed"`
	Branch     string `json:"branch" jsonschema:"Filter by branch name"`
	PerPage    int    `json:"per_page" jsonschema:"Results per page (max 100)"`
	Page       int    `json:"page" jsonschema:"Page number. Default: 1"`
}

func workflowRunsPath(p *listWorkflowRunsParams, q url.Values) string {
	if p.WorkflowID != "" {
		return fmt.Sprintf(
			"/repos/%s/%s/actions/workflows/%s/runs?%s",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo),
			url.PathEscape(p.WorkflowID), q.Encode())
	}
	return fmt.Sprintf("/repos/%s/%s/actions/runs?%s",
		url.PathEscape(p.Owner), url.PathEscape(p.Repo),
		q.Encode())
}

func registerListWorkflowRuns(
	server *mcp.Server, c *client, allowed map[string]bool,
) {
	if len(allowed) > 0 && !allowed["list_workflow_runs"] {
		return
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "list_workflow_runs",
		Description: "Lists workflow runs for a repository.",
	}, func(
		ctx context.Context,
		_ *mcp.CallToolRequest,
		p *listWorkflowRunsParams,
	) (*mcp.CallToolResult, any, error) {
		q := url.Values{}
		if p.Status != "" {
			q.Set("status", p.Status)
		}
		if p.Branch != "" {
			q.Set("branch", p.Branch)
		}
		perPage := p.PerPage
		if perPage <= 0 || perPage > 100 {
			perPage = 30
		}
		q.Set("per_page", strconv.Itoa(perPage))
		if p.Page > 0 {
			q.Set("page", strconv.Itoa(p.Page))
		}
		path := workflowRunsPath(p, q)
		result, err := c.call(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Workflow runs: %s", string(result)))
	})
}

func registerTriggerWorkflow(
	server *mcp.Server, c *client, allowed map[string]bool,
) {
	if len(allowed) > 0 && !allowed["trigger_workflow"] {
		return
	}
	type params struct {
		Owner      string `json:"owner" jsonschema:"required,Repository owner"`
		Repo       string `json:"repo" jsonschema:"required,Repository name"`
		WorkflowID string `json:"workflow_id" jsonschema:"required,Workflow ID or file name"`
		Ref        string `json:"ref" jsonschema:"required,Git ref (branch or tag)"`
		Inputs     string `json:"inputs" jsonschema:"JSON-encoded workflow inputs"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "trigger_workflow",
		Description: "Triggers a workflow dispatch event.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		body := map[string]any{"ref": p.Ref}
		if p.Inputs != "" {
			var inputsMap map[string]string
			if err := json.Unmarshal([]byte(p.Inputs), &inputsMap); err != nil {
				return nil, nil, fmt.Errorf("parsing workflow inputs (must be a JSON object): %w", err)
			}
			body["inputs"] = inputsMap
		}
		path := fmt.Sprintf(
			"/repos/%s/%s/actions/workflows/%s/dispatches",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo),
			url.PathEscape(p.WorkflowID))
		_, err := c.call(ctx, http.MethodPost, path, body)
		if err != nil {
			return nil, nil, err
		}
		return textResult("Workflow dispatch triggered successfully.")
	})
}

func registerGetWorkflowRun(
	server *mcp.Server, c *client, allowed map[string]bool,
) {
	if len(allowed) > 0 && !allowed["get_workflow_run"] {
		return
	}
	type params struct {
		Owner string `json:"owner" jsonschema:"required,Repository owner"`
		Repo  string `json:"repo" jsonschema:"required,Repository name"`
		RunID int64  `json:"run_id" jsonschema:"required,Workflow run ID"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name:        "get_workflow_run",
		Description: "Gets details of a specific workflow run.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		path := fmt.Sprintf("/repos/%s/%s/actions/runs/%d",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo), p.RunID)
		result, err := c.call(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, nil, err
		}
		return textResult(fmt.Sprintf("Workflow run: %s", string(result)))
	})
}

func registerGetRunLogs(
	server *mcp.Server, c *client, allowed map[string]bool,
) {
	if len(allowed) > 0 && !allowed["get_run_logs"] {
		return
	}
	type params struct {
		Owner string `json:"owner" jsonschema:"required,Repository owner"`
		Repo  string `json:"repo" jsonschema:"required,Repository name"`
		RunID int64  `json:"run_id" jsonschema:"required,Workflow run ID"`
	}
	mcp.AddTool(server, &mcp.Tool{
		Name: "get_run_logs",
		Description: "Gets the download URL for a workflow run's logs. " +
			"The GitHub API returns a redirect to a time-limited download URL.",
	}, func(
		ctx context.Context, _ *mcp.CallToolRequest, p *params,
	) (*mcp.CallToolResult, any, error) {
		path := fmt.Sprintf("/repos/%s/%s/actions/runs/%d/logs",
			url.PathEscape(p.Owner), url.PathEscape(p.Repo), p.RunID)
		// GitHub returns a 302 redirect to a temporary download URL for the zip archive.
		// We follow the redirect and return the final URL so the agent can share it.
		downloadURL, err := c.getRedirectURL(ctx, path)
		if err != nil {
			return nil, nil, fmt.Errorf("fetching logs URL for run %d: %w", p.RunID, err)
		}
		return textResult(fmt.Sprintf(
			"Logs download URL (time-limited zip archive): %s", downloadURL))
	})
}
