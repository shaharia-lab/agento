//! `pull_requests`, ported from `internal/integrations/github/pulls.go`.
//!
//! Five tools, and one of them — `get_pull_diff` — is the only caller anywhere
//! in this integration of `callRaw`: a caller-chosen `Accept`, a 10 MB cap
//! rather than 2 MiB, and a response that is not JSON at all.

use schemars::JsonSchema;

use crate::claude::{new_tool, CancellationToken, ToolDef};
use crate::native::gourl::{path_escape, Values};

use super::body::Body;
use super::client::{set_paging, Client};
use super::text_result;

/// `list_pulls`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListPullsInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// Filter: open, closed, all
    state: String,
    /// Sort: created, updated, popularity
    sort: String,
    /// Filter by base branch name
    base: String,
    /// Results per page (max 100)
    per_page: i64,
    /// Page number. Default: 1
    page: i64,
}

pub fn list_pulls(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_pulls",
        "Lists pull requests for a repository.",
        move |input: ListPullsInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut query = Values::new();
                if !input.state.is_empty() {
                    query.set("state", &input.state);
                }
                if !input.sort.is_empty() {
                    query.set("sort", &input.sort);
                }
                if !input.base.is_empty() {
                    query.set("base", &input.base);
                }
                set_paging(&mut query, input.per_page, input.page);
                let path = format!(
                    "/repos/{}/{}/pulls?{}",
                    path_escape(&input.owner),
                    path_escape(&input.repo),
                    query.encode()
                );
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Pull requests: {result}")))
            }
        },
    )
}

/// `get_pull`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetPullInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// required,Pull request number
    number: i64,
}

pub fn get_pull(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_pull",
        "Gets details of a specific pull request by number.",
        move |input: GetPullInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let path = pull_path(&input.owner, &input.repo, input.number);
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Pull request: {result}")))
            }
        },
    )
}

/// `create_pull`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreatePullInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// required,Pull request title
    title: String,
    /// required,Source branch name
    head: String,
    /// required,Target branch name
    base: String,
    /// PR body in Markdown
    body: String,
    /// Create as draft. Default: false
    draft: bool,
}

pub fn create_pull(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "create_pull",
        "Creates a new pull request.",
        move |input: CreatePullInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut body = Body::new();
                body.set("title", input.title.as_str());
                body.set("head", input.head.as_str());
                body.set("base", input.base.as_str());
                body.set_if_non_empty("body", &input.body);
                // `if p.Draft`, so `draft: false` sends no key at all rather
                // than `"draft":false`.
                body.set_if_true("draft", input.draft);
                let path = format!(
                    "/repos/{}/{}/pulls",
                    path_escape(&input.owner),
                    path_escape(&input.repo)
                );
                let result = client
                    .call(&ct, reqwest::Method::POST, &path, Some(body.encode()?))
                    .await?;
                Ok(text_result(format!("Pull request created: {result}")))
            }
        },
    )
}

/// `get_pull_diff`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetPullDiffInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// required,Pull request number
    number: i64,
}

pub fn get_pull_diff(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_pull_diff",
        "Gets the diff of a pull request.",
        move |input: GetPullDiffInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // The same path `get_pull` uses — only the `Accept` differs,
                // which is how the REST API returns a diff instead of JSON.
                let path = pull_path(&input.owner, &input.repo, input.number);
                let result = client
                    .call_raw(
                        &ct,
                        reqwest::Method::GET,
                        &path,
                        "application/vnd.github.v3.diff",
                    )
                    .await?;
                // The one result sentence with a newline rather than a space.
                Ok(text_result(format!("Diff:\n{result}")))
            }
        },
    )
}

/// `list_pull_comments`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListPullCommentsInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// required,Pull request number
    number: i64,
    /// Results per page (max 100)
    per_page: i64,
    /// Page number. Default: 1
    page: i64,
}

pub fn list_pull_comments(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_pull_comments",
        "Lists review comments on a pull request.",
        move |input: ListPullCommentsInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut query = Values::new();
                set_paging(&mut query, input.per_page, input.page);
                let path = format!(
                    "/repos/{}/{}/pulls/{}/comments?{}",
                    path_escape(&input.owner),
                    path_escape(&input.repo),
                    input.number,
                    query.encode()
                );
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Comments: {result}")))
            }
        },
    )
}

/// The path `get_pull` and `get_pull_diff` share, spelled once because they are
/// the same request under two `Accept` headers.
fn pull_path(owner: &str, repo: &str, number: i64) -> String {
    format!(
        "/repos/{}/{}/pulls/{number}",
        path_escape(owner),
        path_escape(repo)
    )
}
