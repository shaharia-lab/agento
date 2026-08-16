//! `repos`, ported from `internal/integrations/github/repos.go`.
//!
//! Three tools. Each is the shape every tool in this module tree takes, so the
//! comments that would repeat verbatim on the other seventeen live here:
//!
//! - The input struct's field names are the Go `json:` tags and its doc
//!   comments are the Go `jsonschema:` tags **verbatim**, `required,` prefix
//!   included. That prefix is not a directive — `google/jsonschema-go` reads
//!   the whole tag as the description — so it is a sentence the model reads and
//!   dropping it would change the advertised surface. See
//!   `crate::claude::schema_vectors` for the generated proof.
//! - Every field is a plain `String` / `i64` / `bool`, never an `Option`: no
//!   params struct in this integration carries `omitempty`, so **every field is
//!   required** and the model must send all of them. An `Option` would render
//!   `["string","null"]` and change the schema.
//! - `deny_unknown_fields` is what emits `"additionalProperties": false`, which
//!   Go's reflector sets on every struct and its server validates against.
//! - The credential is captured by the closure and **cloned per call**, outside
//!   the async block — the `Fn` requirement `crate::claude::tool`'s module docs
//!   spell out.

use schemars::JsonSchema;

use crate::claude::{new_tool, CancellationToken, ToolDef};
use crate::native::gourl::{path_escape, Values};

use super::client::{set_paging, Client};
use super::text_result;

/// `list_repos`.
#[allow(dead_code)] // read through serde, never constructed in Rust
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListReposInput {
    /// Filter by visibility: all, public, private
    visibility: String,
    /// Sort by: created, updated, pushed, full_name
    sort: String,
    /// Results per page (max 100). Default: 30
    per_page: i64,
    /// Page number. Default: 1
    page: i64,
}

pub fn list_repos(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_repos",
        "Lists repositories for the authenticated user.",
        move |input: ListReposInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut query = Values::new();
                // Both filters are dropped when empty, so the default request
                // carries `per_page` alone.
                if !input.visibility.is_empty() {
                    query.set("visibility", &input.visibility);
                }
                if !input.sort.is_empty() {
                    query.set("sort", &input.sort);
                }
                set_paging(&mut query, input.per_page, input.page);
                let path = format!("/user/repos?{}", query.encode());
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Repositories: {result}")))
            }
        },
    )
}

/// `get_repo`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetRepoInput {
    /// required,Repository owner
    owner: String,
    /// required,Repository name
    repo: String,
}

pub fn get_repo(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_repo",
        "Gets details of a specific repository by owner/name.",
        move |input: GetRepoInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // `url.PathEscape`, not the whole-path encoding: a repository
                // named `a/b` has to stay one segment.
                let path = format!(
                    "/repos/{}/{}",
                    path_escape(&input.owner),
                    path_escape(&input.repo)
                );
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Repository: {result}")))
            }
        },
    )
}

/// `search_code`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchCodeInput {
    /// required,Search query
    query: String,
    /// Results per page (max 100). Default: 30
    per_page: i64,
    /// Page number. Default: 1
    page: i64,
}

pub fn search_code(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "search_code",
        "Searches for code across GitHub repositories using a query string.",
        move |input: SearchCodeInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut query = Values::new();
                // Unconditional, unlike every other filter here: an empty
                // search still sends `q=`.
                query.set("q", &input.query);
                set_paging(&mut query, input.per_page, input.page);
                let path = format!("/search/code?{}", query.encode());
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Search results: {result}")))
            }
        },
    )
}
