//! `releases`, ported from `internal/integrations/github/releases.go`.
//!
//! Three tools. `create_release` is the widest conditional body in the
//! integration — one unconditional key and six that appear only when set —
//! which makes it the case that shows `json.Marshal`'s sorting most clearly.

use schemars::JsonSchema;

use crate::claude::{new_tool, CancellationToken, ToolDef};
use crate::native::gourl::{path_escape, Values};

use super::body::Body;
use super::client::{set_paging, Client};
use super::text_result;

/// `list_releases` and `list_tags` share their input shape: owner, repo and
/// paging, with the same three descriptions.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PagedRepoInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// Results per page (max 100)
    per_page: i64,
    /// Page number. Default: 1
    page: i64,
}

pub fn list_releases(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_releases",
        "Lists releases for a repository.",
        move |input: PagedRepoInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let path = paged_repo_path(&input, "releases");
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Releases: {result}")))
            }
        },
    )
}

pub fn list_tags(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_tags",
        "Lists tags for a repository.",
        move |input: PagedRepoInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let path = paged_repo_path(&input, "tags");
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Tags: {result}")))
            }
        },
    )
}

/// `createReleaseParams` — a named type in Go too, because `buildReleaseBody`
/// takes it.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateReleaseInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
    /// required,Tag name
    tag_name: String,
    /// Release name
    name: String,
    /// Release notes in Markdown
    body: String,
    /// Branch or commit SHA
    target_commitish: String,
    /// Create as draft
    draft: bool,
    /// Mark as pre-release
    prerelease: bool,
    /// Auto-generate notes
    generate_release_notes: bool,
}

pub fn create_release(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "create_release",
        "Creates a new release in a repository.",
        move |input: CreateReleaseInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // `buildReleaseBody`, in its order — which does not survive
                // into the request, since `json.Marshal` sorts the keys.
                let mut body = Body::new();
                body.set("tag_name", input.tag_name.as_str());
                body.set_if_non_empty("name", &input.name);
                body.set_if_non_empty("body", &input.body);
                body.set_if_non_empty("target_commitish", &input.target_commitish);
                body.set_if_true("draft", input.draft);
                body.set_if_true("prerelease", input.prerelease);
                body.set_if_true("generate_release_notes", input.generate_release_notes);
                let path = format!(
                    "/repos/{}/{}/releases",
                    path_escape(&input.owner),
                    path_escape(&input.repo)
                );
                let result = client
                    .call(&ct, reqwest::Method::POST, &path, Some(body.encode()?))
                    .await?;
                Ok(text_result(format!("Release created: {result}")))
            }
        },
    )
}

/// `/repos/{owner}/{repo}/{collection}?{paging}` — the shape both list tools
/// build, differing only in the last segment.
fn paged_repo_path(input: &PagedRepoInput, collection: &str) -> String {
    let mut query = Values::new();
    set_paging(&mut query, input.per_page, input.page);
    format!(
        "/repos/{}/{}/{collection}?{}",
        path_escape(&input.owner),
        path_escape(&input.repo),
        query.encode()
    )
}
