//! The Rust half of the reflector divergence map —
//! `desktop/parity/jsonschema_reflect_vectors.json`.
//!
//! #310 verified that a Rust-hosted tool's schema matches a Go-hosted one **for
//! a flat struct of required scalars**, and left a note on
//! [`super::tool::new_tool`] naming the shapes the six integration ports would
//! hit first. This module is that note turned into a test: the Go half
//! (`desktop/parity/jsonschema_reflect_parity_test.go`) reflects one reference
//! struct covering every shape class through `jsonschema.For` — exactly as
//! `mcp.AddTool` does — and this half declares the corresponding Rust shapes,
//! runs them through [`super::new_tool`]'s normalization, and pins **per shape**
//! whether the two agree and what a port must write when they do not.
//!
//! Read this before porting an integration. The answer for each shape is at its
//! site in [`the_divergence_map`], and the summary is:
//!
//! | Go | write in Rust | why |
//! |---|---|---|
//! | `string` + `jsonschema:"…"` | `String` + doc comment | matches |
//! | `string,omitempty` | `#[serde(default)] String` | an `Option<String>` renders `["string","null"]` |
//! | `int` / `int64` | `i64` | matches, once the `format` keyword is dropped |
//! | `int32` / `uint` | still `i64` | Go reflects bounds, `schemars` reflects a format; neither is the other |
//! | `bool` | `bool` | matches |
//! | `[]string` | `Vec<String>` **plus** a `null` in `type` | Go renders `["null","array"]` for every slice |
//! | a nested struct | inline it | Go inlines, `schemars` lifts to `$defs`/`$ref` |
//! | `map[string]string` | `BTreeMap<String, String>` | matches |
//! | `*T` | nothing — no integration uses one | Go renders `["null", …]` |
//!
//! **None of the standing divergences is reachable from #312.** Every params
//! struct in `internal/integrations/github/` is flat, carries no `omitempty`,
//! and uses only `string`, `int`, `int64` and `bool` — which is why the twenty
//! schemas in `github_vectors.json` match exactly, and #317's six in
//! `confluence_vectors.json`, #316's nine in `jira_vectors.json` and #315's seven
//! in `slack_vectors.json` with them. The map exists so #313 does not rediscover
//! the boundary — and #314 is the first port to **reach** one of the standing
//! divergences rather than only read about it: Telegram's `create_poll` takes a
//! `[]string`, so `messaging::go_string_slice` adds the `null` the guidance below
//! says a port that needs one must add itself.

use std::collections::BTreeMap;

use rmcp::model::CallToolResult;
use rmcp::ServerHandler;
use schemars::JsonSchema;
use serde_json::{json, Value};

use super::{new_tool, ToolServer};

/// The nested struct case — `reflectNested` on the Go side.
///
/// `dead_code` is allowed rather than worked around: these types exist to be
/// *reflected*, so nothing ever reads a field. Naming them `_name` would change
/// the property names, which are the whole point.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReflectNested {
    /// The nested name
    name: String,
    /// The nested count
    #[serde(default)]
    count: i64,
}

/// Every shape class, mirroring `reflectReference` field for field.
///
/// The field names are the Go `json:` names so the two documents can be
/// compared property by property, and the doc comments are the Go
/// `jsonschema:` tags verbatim — including the `required,` prefix, which is
/// **not a directive**: `jsonschema-go` reads the whole tag as the description.
#[allow(dead_code)] // reflected, never read — see `ReflectNested`
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReflectReference {
    /// required,A required string
    required_string: String,
    /// An optional string
    #[serde(default)]
    optional_string: String,
    /// An omitzero string
    #[serde(default)]
    omitzero_string: String,
    untagged: String,

    /// A plain int
    int: i64,
    /// An int64
    int64: i64,
    /// An int32
    int32: i64,
    /// A uint
    uint: i64,
    /// A float64
    float: f64,
    /// A bool
    bool: bool,

    /// A string slice
    strings: Vec<String>,
    /// An omitempty string slice
    #[serde(default)]
    strings_omitempty: Vec<String>,

    /// A nested struct
    nested: ReflectNested,
    /// A pointer to a nested struct
    #[serde(default)]
    nested_ptr: Option<ReflectNested>,
    /// A pointer to a string
    ptr_string: Option<String>,

    /// A map of strings
    map: BTreeMap<String, String>,
}

/// The schema `tools/list` would advertise for [`ReflectReference`] — through
/// [`new_tool`], so the normalization under test is the shipped one.
fn rust_schema() -> Value {
    let server = ToolServer::new("reflect").with_tool(new_tool(
        "reflect",
        "The reference struct.",
        |_input: ReflectReference, _ct| async move { Ok(CallToolResult::success(vec![])) },
    ));
    let tool = server.get_tool("reflect").expect("registered");
    serde_json::to_value(&*tool.input_schema).expect("schema")
}

#[derive(serde::Deserialize)]
struct PropertyVector {
    name: String,
    required: bool,
    schema: Value,
}

#[derive(serde::Deserialize)]
struct Vectors {
    required: Vec<String>,
    properties: Vec<PropertyVector>,
}

fn vectors() -> Vectors {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../parity/jsonschema_reflect_vectors.json"
    );
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"));
    serde_json::from_str(&raw).expect("parsing the reflector vectors")
}

/// What a shape's Rust rendering is, relative to Go's.
enum Verdict {
    /// The two documents agree, so a port writes the Rust shape and stops.
    Same,
    /// They do not, and this is exactly what `schemars` produces. Pinned so a
    /// `schemars` upgrade that changes it fails here rather than in a port.
    Differs {
        rust: Value,
        /// Whether `schemars` puts the property in `required`. Spelled out
        /// rather than inherited from Go, because one row differs *only* here:
        /// a Go `*string` with no `omitempty` is **required**, and an
        /// `Option<String>` is not — requiredness and nullability are one
        /// decision in Rust and two in Go.
        rust_required: bool,
        /// What a port must write instead, or why nothing needs to change.
        guidance: &'static str,
    },
}

/// The map itself: one row per Go property, asserted against the frozen Go
/// output *and* against this port's own reading of `schemars`.
///
/// A single test rather than one per shape, because the value being pinned is
/// the *table* — "which shapes are safe" is the question a port asks, and a
/// half-updated table would answer it wrongly while every individual test
/// passed.
#[test]
fn the_divergence_map() {
    use Verdict::{Differs, Same};

    let v = vectors();
    let rust = rust_schema();
    let rust_properties = rust["properties"]
        .as_object()
        .expect("an object schema has properties");
    let rust_required: Vec<String> = rust["required"]
        .as_array()
        .expect("required")
        .iter()
        .map(|name| name.as_str().expect("a string").to_string())
        .collect();

    // In the Go document's order — declaration order — so a reader can hold
    // the two files side by side.
    let verdicts: Vec<(&str, Verdict)> = vec![
        // ── Scalars: the shape #310 already verified, plus the tag rule ──────
        //
        // The description is the Go tag **verbatim**. `required,` is part of
        // the sentence the model reads, not a keyword `jsonschema-go` consumes,
        // and every one of the 62 integration tools writes it that way.
        ("required_string", Same),
        // ── Optionality: the divergence a port is most likely to walk into ───
        //
        // Go's only way to make a field optional is `omitempty`/`omitzero`,
        // which changes the parent's `required` list and nothing else. The
        // matching Rust shape is `#[serde(default)] String` — an
        // `Option<String>` would render `"type": ["string","null"]`, which is a
        // *different type* in front of the model, not a different requiredness.
        ("optional_string", Same),
        ("omitzero_string", Same),
        // An untagged field has no `description` at all, which is a distinct
        // state from an empty one — so a port must leave the doc comment off
        // rather than writing `///`.
        ("untagged", Same),
        // `i64` matches only because `new_tool` drops `schemars`'s `format`
        // keyword; without that these rows would read `{"type":"integer",
        // "format":"int64"}` against Go's bare `{"type":"integer"}`.
        ("int", Same),
        ("int64", Same),
        // ── Sized and unsigned integers: no Rust shape matches ───────────────
        (
            "int32",
            Differs {
                rust: json!({"type": "integer", "description": "An int32"}),
                rust_required: true,
                guidance: "Go reflects an int32 as an integer bounded by \
                           minimum/maximum and `schemars` reflects it as \
                           `format: int32`, which the normalization drops — so \
                           neither spelling reproduces the other. No \
                           integration params struct uses a sized integer; use \
                           `i64` for a Go `int` or `int64`, which is what they \
                           all are.",
            },
        ),
        (
            "uint",
            Differs {
                rust: json!({"type": "integer", "description": "A uint"}),
                rust_required: true,
                guidance: "Go adds `minimum: 0` for an unsigned integer. Rust's \
                           `u64` adds it too, but as an integer where Go's is a \
                           float, so the documents still differ. Same answer: \
                           no integration uses one.",
            },
        ),
        ("float", Same),
        ("bool", Same),
        // ── Slices: Go admits a JSON null for every one of them ──────────────
        (
            "strings",
            Differs {
                rust: json!({
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "A string slice",
                }),
                rust_required: true,
                guidance: "`jsonschema-go` renders every slice as \
                           `[\"null\",\"array\"]`, because a Go nil slice \
                           marshals as `null` and must therefore be accepted \
                           back. `Vec<String>` renders a bare `array`. Nothing \
                           in the six integrations takes a list — every \
                           multi-value input is a comma-separated `string` \
                           split by `splitCSV` — so this is documented rather \
                           than reconciled; a port that needs one must add the \
                           null itself.",
            },
        ),
        (
            "strings_omitempty",
            Differs {
                rust: json!({
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "An omitempty string slice",
                }),
                rust_required: false,
                guidance: "Same as `strings`: `omitempty` moves the field out \
                           of `required` and leaves the `null` in the type.",
            },
        ),
        // ── Nested structs: Go inlines, schemars lifts ───────────────────────
        (
            "nested",
            Differs {
                rust: json!({
                    "$ref": "#/$defs/ReflectNested",
                    "description": "A nested struct",
                }),
                rust_required: true,
                guidance: "`schemars` lifts a nested struct into `$defs` and \
                           points a `$ref` at it; Go inlines it at every use. \
                           `new_tool` deliberately does not de-reference — \
                           inlining is a rewrite with real choices in it \
                           (sibling keywords, recursion) and no caller needs it \
                           yet: every params struct in the six integrations is \
                           flat. A port that needs nesting must flatten the \
                           struct instead.",
            },
        ),
        (
            "nested_ptr",
            Differs {
                rust: json!({
                    "anyOf": [
                        {"$ref": "#/$defs/ReflectNested"},
                        {"type": "null"},
                    ],
                    "description": "A pointer to a nested struct",
                }),
                rust_required: false,
                guidance: "The two divergences compounded: `$defs` for the \
                           nesting and `anyOf` + `null` for the pointer, \
                           against Go's inlined `[\"null\",\"object\"]`.",
            },
        ),
        (
            "ptr_string",
            Differs {
                rust: json!({
                    "type": ["string", "null"],
                    "description": "A pointer to a string",
                }),
                rust_required: false,
                guidance: "Go orders the union `[\"null\",\"string\"]` and \
                           `schemars` orders it `[\"string\",\"null\"]` — the \
                           same JSON Schema, different bytes, and the Rust test \
                           for a real tool compares documents. No integration \
                           takes a pointer field; this row exists so the \
                           ordering is on record.",
            },
        ),
        // ── Maps: the one container that matches outright ────────────────────
        ("map", Same),
    ];

    assert_eq!(
        verdicts.len(),
        v.properties.len(),
        "every Go property needs a verdict — the map is the thing being pinned"
    );

    for ((name, verdict), want) in verdicts.into_iter().zip(&v.properties) {
        assert_eq!(name, want.name, "the map is in the Go document's order");
        let got = rust_properties
            .get(name)
            .unwrap_or_else(|| panic!("{name} is not in the Rust schema"));

        // Requiredness is the parent's business in both languages, and it is
        // the half `Option<T>` gets wrong: a `#[serde(default)] String` leaves
        // `required` and keeps its type, which is exactly `omitempty`.
        let is_required = rust_required.contains(&name.to_string());

        match verdict {
            Same => {
                assert_eq!(
                    is_required, want.required,
                    "{name}: mapped as matching Go, and its requiredness no \
                     longer does (Go's required list: {:?})",
                    v.required
                );
                assert_eq!(
                    got, &want.schema,
                    "{name} was mapped as matching Go, and no longer does"
                );
            }
            Differs {
                rust,
                rust_required: want_required,
                guidance,
            } => {
                assert_eq!(
                    is_required, want_required,
                    "{name}: schemars no longer agrees with the map about \
                     requiredness. Guidance on file: {guidance}"
                );
                assert_ne!(
                    got, &want.schema,
                    "{name} was mapped as diverging and now matches Go — \
                     delete the row and its guidance: {guidance}"
                );
                assert_eq!(
                    got, &rust,
                    "{name}: schemars no longer produces what the map records. \
                     Guidance on file: {guidance}"
                );
            }
        }
    }
}

/// The `$defs` block `schemars` adds and Go never has, asserted where a reader
/// will look for it — the nested rows above only show the `$ref` side.
#[test]
fn a_nested_struct_leaves_a_defs_block_behind() {
    let rust = rust_schema();
    assert!(
        rust.get("$defs").is_some(),
        "the nested-struct divergence is a document-level one too: {rust}"
    );
    assert_eq!(rust["$defs"]["ReflectNested"]["type"], "object");
    // …and no dialect key survived, on the nested schema either.
    assert!(rust.get("$schema").is_none());
    assert!(rust["$defs"]["ReflectNested"].get("$schema").is_none());
}
