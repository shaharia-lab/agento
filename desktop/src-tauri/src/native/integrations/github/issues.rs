//! `issues`, ported from `internal/integrations/github/issues.go`.
//!
//! Four tools, and the two writers are where `json.Marshal`'s key sorting and
//! HTML escaping first become observable — see [`super::body`].

use schemars::JsonSchema;

use crate::claude::{new_tool, CancellationToken, ToolDef};
use crate::native::gourl::{path_escape, Values};

use super::body::Body;
use super::client::{set_paging, Client};
use super::text_result;

/// `list_issues`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListIssuesInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// Filter: open, closed, all. Default: open
    state: String,
    /// Comma-separated label names
    labels: String,
    /// Sort: created, updated, comments
    sort: String,
    /// Results per page (max 100)
    per_page: i64,
    /// Page number. Default: 1
    page: i64,
}

pub fn list_issues(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_issues",
        "Lists issues for a repository.",
        move |input: ListIssuesInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut query = Values::new();
                if !input.state.is_empty() {
                    query.set("state", &input.state);
                }
                // Passed through whole, not split: `splitCSV` is only for
                // request *bodies*. GitHub takes the comma-separated form here,
                // and `Values.Encode` escapes the comma to `%2C`.
                if !input.labels.is_empty() {
                    query.set("labels", &input.labels);
                }
                if !input.sort.is_empty() {
                    query.set("sort", &input.sort);
                }
                set_paging(&mut query, input.per_page, input.page);
                let path = format!(
                    "/repos/{}/{}/issues?{}",
                    path_escape(&input.owner),
                    path_escape(&input.repo),
                    query.encode()
                );
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Issues: {result}")))
            }
        },
    )
}

/// `get_issue`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetIssueInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// required,Issue number
    number: i64,
}

pub fn get_issue(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_issue",
        "Gets details of a specific issue by number.",
        move |input: GetIssueInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // `%d`, not `PathEscape`: the number is already a number, and
                // Go interpolates it directly.
                let path = format!(
                    "/repos/{}/{}/issues/{}",
                    path_escape(&input.owner),
                    path_escape(&input.repo),
                    input.number
                );
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Issue: {result}")))
            }
        },
    )
}

/// `create_issue`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateIssueInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// required,Issue title
    title: String,
    /// Issue body in Markdown
    body: String,
    /// Comma-separated label names
    labels: String,
    /// Comma-separated usernames
    assignees: String,
}

pub fn create_issue(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "create_issue",
        "Creates a new issue in a repository.",
        move |input: CreateIssueInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // `title` is unconditional; everything else is conditional. So
                // the minimal request is exactly `{"title":"…"}`.
                let mut body = Body::new();
                body.set("title", input.title.as_str());
                body.set_if_non_empty("body", &input.body);
                body.set_csv("labels", &input.labels);
                body.set_csv("assignees", &input.assignees);
                let path = format!(
                    "/repos/{}/{}/issues",
                    path_escape(&input.owner),
                    path_escape(&input.repo)
                );
                let result = client
                    .call(&ct, reqwest::Method::POST, &path, Some(body.encode()?))
                    .await?;
                Ok(text_result(format!("Issue created: {result}")))
            }
        },
    )
}

/// `update_issue`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateIssueInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// required,Issue number
    number: i64,
    /// New title
    title: String,
    /// New body in Markdown
    body: String,
    /// New state: open or closed
    state: String,
    /// Comma-separated label names
    labels: String,
}

pub fn update_issue(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "update_issue",
        "Updates an existing issue.",
        move |input: UpdateIssueInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // Nothing is unconditional here, so an update with every field
                // empty sends the two bytes `{}` — and, because there *is* a
                // body, still sends `Content-Type: application/json`.
                let mut body = Body::new();
                body.set_if_non_empty("title", &input.title);
                body.set_if_non_empty("body", &input.body);
                body.set_if_non_empty("state", &input.state);
                body.set_csv("labels", &input.labels);
                let path = format!(
                    "/repos/{}/{}/issues/{}",
                    path_escape(&input.owner),
                    path_escape(&input.repo),
                    input.number
                );
                let result = client
                    .call(&ct, reqwest::Method::PATCH, &path, Some(body.encode()?))
                    .await?;
                Ok(text_result(format!("Issue updated: {result}")))
            }
        },
    )
}
