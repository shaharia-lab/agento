//! The `project_management` service's nine tools, ported from
//! `internal/integrations/jira/tools.go`.
//!
//! Jira has one service group registered in four batches (`registerProjectTools`,
//! `registerIssueReadTools`, `registerIssueMutationTools`,
//! `registerTransitionTools`), so this file is the whole tool surface. The
//! conventions are `native/integrations/github/repos.rs`'s — Go `json:` tags as
//! field names, Go `jsonschema:` tags as doc comments **verbatim** (`required,`
//! prefix included, because `jsonschema-go` reads the whole tag as the
//! description), no `Option` anywhere since no params struct carries `omitempty`,
//! and `deny_unknown_fields` for the `additionalProperties: false` Go's reflector
//! emits.
//!
//! # Four things here that look like mistakes and are Go's behaviour
//!
//! Each is pinned by `desktop/parity/jira_vectors.json`, because a port that
//! "fixed" any of them would change the wire:
//!
//! 1. **`list_projects` takes no arguments at all.** Its Go handler binds
//!    `*struct{}`, so the advertised schema is an empty object — the only tool in
//!    any of the six integrations with no fields.
//! 2. **`create_issue` percent-escapes the project key *inside the JSON body*.**
//!    `"project": {"key": url.PathEscape(params.ProjectKey)}` — a path escape
//!    applied to a value that never goes in a path. A key holding a space is
//!    therefore sent as `MY%20PROJ` in the body. Reproduced with
//!    [`gourl::path_escape`], and note it is *only* the project key: `issuetype`,
//!    `summary`, `priority` and every other body value are raw.
//! 3. **`update_issue` and `transition_issue` discard the response.** Their
//!    result text is built from the *arguments* (`Issue %s updated successfully.`)
//!    rather than from what Jira answered, so a 200 with a surprising body still
//!    reads as success — and `update_issue` with nothing set sends
//!    `{"fields":{}}`, which still counts as a body and so still sends
//!    `Content-Type`.
//! 4. **`/rest/api/3/issue/` carries its trailing slash in the constant.** So
//!    `get_issue` builds `/rest/api/3/issue/KEY` while `get_project` has to add
//!    its own slash to `/rest/api/3/project`. Same shape on the wire, two
//!    different spellings in the source.
//!
//! # Atlassian Document Format
//!
//! `docBody` wraps plain text in ADF, and it is the only nested structure any
//! Jira body carries: a `doc` of `version` 1 whose one `paragraph` holds one
//! `text` node. `version` is an integer, and `json.Marshal` sorts every level, so
//! the encoded shape is not the source's field order.

use schemars::JsonSchema;
use serde_json::{json, Value};

use crate::claude::{new_tool, CancellationToken, ToolDef};
use crate::native::gourl::path_escape;

use super::client::{clamp_max_results, Client};
use super::text_result;

/// `jiraAPIIssue` — note the **trailing slash**, which is why callers append a
/// key directly.
const API_ISSUE: &str = "/rest/api/3/issue/";
/// `jiraAPIProject` — no trailing slash, so `get_project` adds one.
const API_PROJECT: &str = "/rest/api/3/project";

/// `json.Marshal(body)`, which `(*client).call` does inside itself in Go.
///
/// Go's wording is kept even though this cannot fail for any value built here —
/// every one is a string, an integer or a map of those.
fn marshal(payload: &Value) -> Result<Vec<u8>, String> {
    crate::native::gojson::to_vec_marshal(payload)
        .map_err(|e| format!("marshaling request body: {e}"))
}

/// `docBody`: plain text as an Atlassian Document Format document.
fn doc_body(text: &str) -> Value {
    json!({
        "type": "doc",
        "version": 1,
        "content": [{
            "type": "paragraph",
            "content": [{"type": "text", "text": text}],
        }],
    })
}

/// `list_projects`.
///
/// The Go handler binds `*struct{}`, so this struct has no fields and the
/// advertised schema is an empty object. It still derives `deny_unknown_fields`,
/// which is what refuses a caller that sends anything.
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListProjectsInput {}

pub fn list_projects(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_projects",
        "Lists all accessible Jira projects.",
        move |_input: ListProjectsInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let result = client
                    .call(&ct, reqwest::Method::GET, API_PROJECT, None)
                    .await?;
                Ok(text_result(format!("Projects: {result}")))
            }
        },
    )
}

/// `get_project`.
#[allow(dead_code)] // read through serde, never constructed in Rust
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetProjectInput {
    /// required,The project key (e.g. PROJ)
    key: String,
}

pub fn get_project(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_project",
        "Gets details of a specific Jira project by key.",
        move |input: GetProjectInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // `jiraAPIProject + "/" + url.PathEscape(key)` — the slash is the
                // caller's here, because this constant has none.
                let path = format!("{API_PROJECT}/{}", path_escape(&input.key));
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Project: {result}")))
            }
        },
    )
}

/// `search_issues`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchIssuesInput {
    /// required,JQL query string (e.g. project = PROJ AND status = Open)
    jql: String,
    /// Maximum number of issues to return (default 50, max 100)
    max_results: i64,
}

pub fn search_issues(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "search_issues",
        "Searches Jira issues using JQL (Jira Query Language).",
        move |input: SearchIssuesInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // JQL travels in the **body**, so nothing escapes it — unlike
                // Confluence's CQL, which is a query parameter.
                let payload = json!({
                    "jql": input.jql,
                    "maxResults": clamp_max_results(input.max_results),
                });
                let result = client
                    .call(
                        &ct,
                        reqwest::Method::POST,
                        "/rest/api/3/search",
                        Some(marshal(&payload)?),
                    )
                    .await?;
                Ok(text_result(format!("Search results: {result}")))
            }
        },
    )
}

/// `get_issue`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetIssueInput {
    /// required,The issue key (e.g. PROJ-123)
    key: String,
}

pub fn get_issue(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_issue",
        "Gets details of a specific Jira issue by key (e.g. PROJ-123).",
        move |input: GetIssueInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // The constant already ends in `/`.
                let path = format!("{API_ISSUE}{}", path_escape(&input.key));
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
    /// required,The project key (e.g. PROJ)
    project_key: String,
    /// required,Issue type name (e.g. Bug, Story, Task)
    issue_type: String,
    /// required,Summary/title of the issue
    summary: String,
    /// Optional description of the issue
    description: String,
    /// Optional priority name (e.g. High, Medium, Low)
    priority: String,
}

pub fn create_issue(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "create_issue",
        "Creates a new Jira issue in a project.",
        move |input: CreateIssueInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut fields = json!({
                    // `url.PathEscape` on a **body** value. Not a mistake to
                    // correct: see the module header. Only the project key.
                    "project": {"key": path_escape(&input.project_key)},
                    "issuetype": {"name": input.issue_type},
                    "summary": input.summary,
                });
                if !input.description.is_empty() {
                    fields["description"] = doc_body(&input.description);
                }
                if !input.priority.is_empty() {
                    fields["priority"] = json!({"name": input.priority});
                }

                let result = client
                    .call(
                        &ct,
                        reqwest::Method::POST,
                        "/rest/api/3/issue",
                        Some(marshal(&json!({"fields": fields}))?),
                    )
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
    /// required,The issue key (e.g. PROJ-123)
    key: String,
    /// Optional new summary/title
    summary: String,
    /// Optional new description
    description: String,
    /// Optional new priority name (e.g. High, Medium, Low)
    priority: String,
}

pub fn update_issue(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "update_issue",
        "Updates fields of an existing Jira issue.",
        move |input: UpdateIssueInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // Every field is conditional, so an all-empty update sends
                // `{"fields":{}}` — still a body, and therefore still a
                // `Content-Type`.
                let mut fields = json!({});
                if !input.summary.is_empty() {
                    fields["summary"] = Value::String(input.summary.clone());
                }
                if !input.description.is_empty() {
                    fields["description"] = doc_body(&input.description);
                }
                if !input.priority.is_empty() {
                    fields["priority"] = json!({"name": input.priority});
                }

                let path = format!("{API_ISSUE}{}", path_escape(&input.key));
                // The response is **discarded**: the sentence is built from the
                // argument, so it reads the same whatever Jira answered.
                client
                    .call(
                        &ct,
                        reqwest::Method::PUT,
                        &path,
                        Some(marshal(&json!({"fields": fields}))?),
                    )
                    .await?;
                Ok(text_result(format!(
                    "Issue {} updated successfully.",
                    input.key
                )))
            }
        },
    )
}

/// `add_comment`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AddCommentInput {
    /// required,The issue key (e.g. PROJ-123)
    key: String,
    /// required,The text of the comment to add
    comment: String,
}

pub fn add_comment(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "add_comment",
        "Adds a comment to a Jira issue.",
        move |input: AddCommentInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let payload = json!({"body": doc_body(&input.comment)});
                let path = format!("{API_ISSUE}{}/comment", path_escape(&input.key));
                let result = client
                    .call(&ct, reqwest::Method::POST, &path, Some(marshal(&payload)?))
                    .await?;
                Ok(text_result(format!("Comment added: {result}")))
            }
        },
    )
}

/// `list_transitions`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListTransitionsInput {
    /// required,The issue key (e.g. PROJ-123)
    key: String,
}

pub fn list_transitions(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_transitions",
        "Lists available status transitions for a Jira issue.",
        move |input: ListTransitionsInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let path = format!("{API_ISSUE}{}/transitions", path_escape(&input.key));
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Transitions: {result}")))
            }
        },
    )
}

/// `transition_issue`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TransitionIssueInput {
    /// required,The issue key (e.g. PROJ-123)
    key: String,
    /// required,The ID of the transition to perform
    transition_id: String,
}

pub fn transition_issue(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "transition_issue",
        "Transitions a Jira issue to a new status.",
        move |input: TransitionIssueInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let payload = json!({"transition": {"id": input.transition_id}});
                let path = format!("{API_ISSUE}{}/transitions", path_escape(&input.key));
                // Response discarded, like `update_issue`'s.
                client
                    .call(&ct, reqwest::Method::POST, &path, Some(marshal(&payload)?))
                    .await?;
                Ok(text_result(format!(
                    "Issue {} transitioned successfully.",
                    input.key
                )))
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(payload: &Value) -> String {
        String::from_utf8(marshal(payload).expect("encode")).expect("utf-8")
    }

    /// ADF, sorted at every level by `json.Marshal` — so the encoded shape is not
    /// `docBody`'s source order, and `version` is an integer.
    #[test]
    fn a_doc_body_is_sorted_and_its_version_is_an_integer() {
        assert_eq!(
            encoded(&doc_body("hello")),
            concat!(
                r#"{"content":[{"content":[{"text":"hello","type":"text"}],"#,
                r#""type":"paragraph"}],"type":"doc","version":1}"#
            )
        );
    }

    /// The HTML escaping `json.Marshal` applies and `serde_json` does not. A Jira
    /// summary or comment is prose a person wrote, so this fires constantly.
    #[test]
    fn body_text_is_html_escaped_the_way_go_escapes_it() {
        assert!(encoded(&doc_body("a <b> & c")).contains(r"a \u003cb\u003e \u0026 c"));
    }

    /// An all-empty update is still a body — which is what makes it send a
    /// `Content-Type`.
    #[test]
    fn an_empty_update_sends_an_empty_fields_object() {
        assert_eq!(encoded(&json!({"fields": json!({})})), r#"{"fields":{}}"#);
    }
}
