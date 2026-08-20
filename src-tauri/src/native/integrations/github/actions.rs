//! `actions`, ported from `internal/integrations/github/actions.go`.
//!
//! Five tools, and the two that are unlike anything else in this integration:
//!
//! - `trigger_workflow` parses a **caller-supplied JSON document**, so
//!   `encoding/json`'s own error wording reaches the model. See
//!   [`super::body::parse_string_map`] for what is reproduced and the two
//!   malformed-input cases that are not.
//! - `get_run_logs` is the only user of the no-redirect client: the API answers
//!   a 302 and the *header* is the answer.

use schemars::JsonSchema;

use crate::claude::{new_tool, CancellationToken, ToolDef};
use crate::native::gourl::{path_escape, Values};

use super::body::{parse_string_map, Body};
use super::client::{set_paging, Client};
use super::text_result;

/// `list_workflows`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListWorkflowsInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// Results per page (max 100)
    per_page: i64,
    /// Page number. Default: 1
    page: i64,
}

pub fn list_workflows(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_workflows",
        "Lists all workflows in a repository.",
        move |input: ListWorkflowsInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut query = Values::new();
                set_paging(&mut query, input.per_page, input.page);
                let path = format!(
                    "/repos/{}/{}/actions/workflows?{}",
                    path_escape(&input.owner),
                    path_escape(&input.repo),
                    query.encode()
                );
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Workflows: {result}")))
            }
        },
    )
}

/// `listWorkflowRunsParams` — a named type in Go too, because
/// `workflowRunsPath` takes it.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListWorkflowRunsInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// Workflow ID or file name
    workflow_id: String,
    /// Filter: queued, in_progress, completed
    status: String,
    /// Filter by branch name
    branch: String,
    /// Results per page (max 100)
    per_page: i64,
    /// Page number. Default: 1
    page: i64,
}

pub fn list_workflow_runs(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_workflow_runs",
        "Lists workflow runs for a repository.",
        move |input: ListWorkflowRunsInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut query = Values::new();
                if !input.status.is_empty() {
                    query.set("status", &input.status);
                }
                if !input.branch.is_empty() {
                    query.set("branch", &input.branch);
                }
                set_paging(&mut query, input.per_page, input.page);
                // `workflowRunsPath`: a workflow id picks a different endpoint
                // entirely rather than adding a parameter, and it is
                // `PathEscape`'d because it may be a file name.
                let path = if input.workflow_id.is_empty() {
                    format!(
                        "/repos/{}/{}/actions/runs?{}",
                        path_escape(&input.owner),
                        path_escape(&input.repo),
                        query.encode()
                    )
                } else {
                    format!(
                        "/repos/{}/{}/actions/workflows/{}/runs?{}",
                        path_escape(&input.owner),
                        path_escape(&input.repo),
                        path_escape(&input.workflow_id),
                        query.encode()
                    )
                };
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Workflow runs: {result}")))
            }
        },
    )
}

/// `trigger_workflow`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TriggerWorkflowInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// required,Workflow ID or file name
    workflow_id: String,
    /// required,Git ref (branch or tag)
    #[serde(rename = "ref")]
    git_ref: String,
    /// JSON-encoded workflow inputs
    inputs: String,
}

pub fn trigger_workflow(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "trigger_workflow",
        "Triggers a workflow dispatch event.",
        move |input: TriggerWorkflowInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut body = Body::new();
                body.set("ref", input.git_ref.as_str());
                if !input.inputs.is_empty() {
                    // The parse failure is the **only** path in this
                    // integration that answers without making a request, which
                    // is why `github_vectors.json` carries a null `request` for
                    // exactly these cases.
                    let parsed = parse_string_map(&input.inputs).map_err(|e| {
                        format!("parsing workflow inputs (must be a JSON object): {e}")
                    })?;
                    // A literal `null` parses to a **nil map** and Go stores it
                    // anyway, so the request carries `"inputs":null`.
                    body.set(
                        "inputs",
                        match parsed {
                            Some(map) => serde_json::to_value(map)
                                .map_err(|e| format!("marshaling request body: {e}"))?,
                            None => serde_json::Value::Null,
                        },
                    );
                }
                let path = format!(
                    "/repos/{}/{}/actions/workflows/{}/dispatches",
                    path_escape(&input.owner),
                    path_escape(&input.repo),
                    path_escape(&input.workflow_id)
                );
                // The response body is discarded — the API answers 204 — so the
                // sentence is a constant.
                client
                    .call(&ct, reqwest::Method::POST, &path, Some(body.encode()?))
                    .await?;
                Ok(text_result(
                    "Workflow dispatch triggered successfully.".to_string(),
                ))
            }
        },
    )
}

/// `get_workflow_run` and `get_run_logs` share their input shape — `run_id` is
/// an `int64` in Go, which is `i64` here and reflects to a bare `integer` once
/// `new_tool` drops `schemars`'s `format` keyword.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// required,Workflow run ID
    run_id: i64,
}

pub fn get_workflow_run(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_workflow_run",
        "Gets details of a specific workflow run.",
        move |input: RunInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let path = format!(
                    "/repos/{}/{}/actions/runs/{}",
                    path_escape(&input.owner),
                    path_escape(&input.repo),
                    input.run_id
                );
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Workflow run: {result}")))
            }
        },
    )
}

pub fn get_run_logs(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_run_logs",
        "Gets the download URL for a workflow run's logs. \
         The GitHub API returns a redirect to a time-limited download URL.",
        move |input: RunInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let path = format!(
                    "/repos/{}/{}/actions/runs/{}/logs",
                    path_escape(&input.owner),
                    path_escape(&input.repo),
                    input.run_id
                );
                // The one tool that wraps its client error rather than
                // returning it — so a 500 here reads
                // `fetching logs URL for run 42: github API error: status 500: …`
                // and the same 500 from any other tool does not.
                let download_url = client
                    .get_redirect_url(&ct, &path)
                    .await
                    .map_err(|e| format!("fetching logs URL for run {}: {e}", input.run_id))?;
                Ok(text_result(format!(
                    "Logs download URL (time-limited zip archive): {download_url}"
                )))
            }
        },
    )
}
