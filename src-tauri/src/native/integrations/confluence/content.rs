//! The `content` service's six tools, ported from
//! `internal/integrations/confluence/tools.go`.
//!
//! Confluence has one service group, so this file is the whole tool surface.
//! Each tool is the shape every ported tool takes, and the conventions behind it
//! are `native/integrations/github/repos.rs`'s:
//!
//! - The input struct's field names are the Go `json:` tags and its doc comments
//!   are the Go `jsonschema:` tags **verbatim**, `required,` prefix included.
//!   That prefix is not a directive — `google/jsonschema-go` reads the whole tag
//!   as the description — so it is a sentence the model reads and dropping it
//!   would change the advertised surface. See `crate::claude::schema_vectors`
//!   for the generated proof.
//! - Every field is a plain `String` / `i64`, never an `Option`: no params
//!   struct in this integration carries `omitempty`, so **every field is
//!   required** and the model must send all of them. An `Option` would render
//!   `["string","null"]` and change the schema.
//! - `deny_unknown_fields` is what emits `"additionalProperties": false`, which
//!   Go's reflector sets on every struct and its server validates against.
//! - The credential is captured by the closure and **cloned per call**, outside
//!   the async block — the `Fn` requirement `crate::claude::tool`'s module docs
//!   spell out.
//!
//! # The two request bodies
//!
//! `create_page` and `update_page` build a `map[string]any` — nested, unlike
//! anything in the GitHub port — and hand it to `json.Marshal`. Three properties
//! of that encoding are reproduced by [`crate::native::gojson::to_vec_marshal`]
//! over a `BTreeMap`, and all three are visible in the vectors:
//!
//! - **Keys are sorted, at every level.** `encoding/json` sorts map keys;
//!   `BTreeMap` iterates sorted and `serde_json::Map` is one too, so the nested
//!   `body` and `version` objects sort as well.
//! - **`<`, `>` and `&` are escaped** to `\u003c`, `\u003e`, `\u0026`. A page
//!   body is Confluence *storage format* — XHTML — so this fires on every single
//!   `create_page`, and `serde_json` on its own emits those three bytes
//!   literally.
//! - **`parentId` is written only when non-empty**, which is Go's one condition
//!   here; `update_page` has none and always sends all five keys.

use schemars::JsonSchema;
use serde_json::{json, Value};

use crate::claude::{new_tool, CancellationToken, ToolDef};
use crate::native::gourl::{path_escape, query_escape};

use super::client::{clamp_limit, Client};
use super::text_result;

/// `json.Marshal(payload)` for a Go `map[string]any`.
///
/// Infallible in practice — every value here is a string, an integer or a map of
/// those — but the encoder's signature is fallible, and Go's own
/// `fmt.Errorf("marshaling request: %w", err)` is what the model would read, so
/// the wording is kept.
fn marshal(payload: &Value) -> Result<Vec<u8>, String> {
    crate::native::gojson::to_vec_marshal(payload).map_err(|e| format!("marshaling request: {e}"))
}

/// `list_spaces`.
#[allow(dead_code)] // read through serde, never constructed in Rust
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListSpacesInput {
    /// Maximum number of spaces to return (1-250)
    limit: i64,
}

pub fn list_spaces(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_spaces",
        "Lists Confluence spaces.",
        move |input: ListSpacesInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // `fmt.Sprintf`, not `url.Values` — so the query is these two
                // literal bytes and a decimal, with no escaping in play.
                let limit = clamp_limit(input.limit, 50);
                let path = format!("/wiki/api/v2/spaces?limit={limit}");
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Spaces: {result}")))
            }
        },
    )
}

/// `get_space`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetSpaceInput {
    /// required,The ID of the space to retrieve
    space_id: String,
}

pub fn get_space(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_space",
        "Gets details of a Confluence space by ID.",
        move |input: GetSpaceInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // `url.PathEscape`, not the whole-path encoding: a space id
                // holding a `/` has to stay one segment.
                let path = format!("/wiki/api/v2/spaces/{}", path_escape(&input.space_id));
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Space: {result}")))
            }
        },
    )
}

/// `search_content`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchContentInput {
    /// required,CQL query string (e.g. 'space = DEV AND type = page')
    cql: String,
    /// Maximum number of results to return (1-250)
    limit: i64,
}

pub fn search_content(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "search_content",
        "Searches Confluence content using CQL (Confluence Query Language).",
        move |input: SearchContentInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // `url.QueryEscape`, which is `encodeQueryComponent`: a space
                // becomes `+`, not `%20`. CQL is full of spaces and `=`, so the
                // difference is on every real query.
                let limit = clamp_limit(input.limit, 25);
                let path = format!(
                    "/wiki/api/v2/search?cql={}&limit={limit}",
                    query_escape(&input.cql)
                );
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Search results: {result}")))
            }
        },
    )
}

/// `get_page`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetPageInput {
    /// required,The ID of the page to retrieve
    page_id: String,
    /// Body format to return: storage or view (default: storage)
    body_format: String,
}

pub fn get_page(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_page",
        "Gets the content and metadata of a Confluence page by ID.",
        move |input: GetPageInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // The default is applied on **empty**, not on an unrecognized
                // value: anything else is passed through and Confluence answers
                // for it.
                let format = if input.body_format.is_empty() {
                    "storage"
                } else {
                    &input.body_format
                };
                let path = format!(
                    "/wiki/api/v2/pages/{}?body-format={}",
                    path_escape(&input.page_id),
                    query_escape(format)
                );
                let result = client.call(&ct, reqwest::Method::GET, &path, None).await?;
                Ok(text_result(format!("Page: {result}")))
            }
        },
    )
}

/// `create_page`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreatePageInput {
    /// required,The ID of the space to create the page in
    space_id: String,
    /// required,Title of the new page
    title: String,
    /// required,Page body content in Confluence storage format (XHTML)
    body: String,
    /// Optional parent page ID to nest under
    parent_id: String,
}

pub fn create_page(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "create_page",
        "Creates a new Confluence page in a given space.",
        move |input: CreatePageInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut payload = json!({
                    "spaceId": input.space_id,
                    "status": "current",
                    "title": input.title,
                    "body": {
                        "representation": "storage",
                        "value": input.body,
                    },
                });
                // The one condition: an empty parent leaves no key at all,
                // rather than sending `"parentId":""`.
                if !input.parent_id.is_empty() {
                    payload["parentId"] = Value::String(input.parent_id.clone());
                }
                let encoded = marshal(&payload)?;

                let result = client
                    .call(
                        &ct,
                        reqwest::Method::POST,
                        "/wiki/api/v2/pages",
                        Some(encoded),
                    )
                    .await?;
                Ok(text_result(format!("Page created: {result}")))
            }
        },
    )
}

/// `update_page`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdatePageInput {
    /// required,The ID of the page to update
    page_id: String,
    /// required,New title for the page
    title: String,
    /// required,New page body content in Confluence storage format (XHTML)
    body: String,
    /// required,Current version number of the page (incremented by 1 on update)
    version: i64,
}

pub fn update_page(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "update_page",
        "Updates an existing Confluence page.",
        move |input: UpdatePageInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // `params.Version + 1` on a Go `int`, which is 64-bit here — so
                // this wraps where Go's wraps and the vectors pin it. No clamp
                // and no validation: Confluence answers for whatever arrives.
                let payload = json!({
                    "id": input.page_id,
                    "status": "current",
                    "title": input.title,
                    "body": {
                        "representation": "storage",
                        "value": input.body,
                    },
                    "version": {"number": input.version.wrapping_add(1)},
                });
                let encoded = marshal(&payload)?;

                let path = format!("/wiki/api/v2/pages/{}", path_escape(&input.page_id));
                let result = client
                    .call(&ct, reqwest::Method::PUT, &path, Some(encoded))
                    .await?;
                Ok(text_result(format!("Page updated: {result}")))
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sorted keys at every level and HTML escaping, which is the whole of
    /// `json.Marshal` over a Go map — and the reason `create_page`'s body is not
    /// in field order.
    #[test]
    fn a_body_is_sorted_and_html_escaped_at_every_level() {
        let payload = json!({
            "spaceId": "SPACE",
            "status": "current",
            "title": "Release <1.0> & notes",
            "body": {"representation": "storage", "value": "<p>hi &amp; bye</p>"},
        });
        assert_eq!(
            String::from_utf8(marshal(&payload).expect("encode")).expect("utf-8"),
            concat!(
                r#"{"body":{"representation":"storage","#,
                r#""value":"\u003cp\u003ehi \u0026amp; bye\u003c/p\u003e"},"#,
                r#""spaceId":"SPACE","status":"current","#,
                r#""title":"Release \u003c1.0\u003e \u0026 notes"}"#
            )
        );
    }

    /// An integer is an integer, not a float — `version.number` is the only
    /// number either body carries and a `30.0` there would be a different
    /// request.
    #[test]
    fn a_version_number_is_spelled_as_an_integer() {
        let payload = json!({"version": {"number": 41_i64.wrapping_add(1)}});
        assert_eq!(
            String::from_utf8(marshal(&payload).expect("encode")).expect("utf-8"),
            r#"{"version":{"number":42}}"#
        );
    }
}
