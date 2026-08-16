//! Request bodies, the way `internal/integrations/github` builds them.
//!
//! Four tools POST or PATCH a body — `create_issue`, `update_issue`,
//! `create_pull`, `create_release` — and every one of them builds a
//! `map[string]any` conditionally and hands it to `json.Marshal`. The bytes
//! that produces are a parity surface even though nothing in a *response*
//! reflects them, which is why `desktop/parity/github_vectors.json` records the
//! body the fake GitHub received.
//!
//! Three properties come from `json.Marshal` over a Go map, and all three are
//! reproduced by [`crate::native::gojson::to_vec_marshal`] over a `BTreeMap`:
//!
//! - **Keys are sorted.** `encoding/json` sorts map keys; `BTreeMap` iterates
//!   sorted. A `struct` would have given declaration order, which is why this
//!   is a map here as it is there.
//! - **`<`, `>` and `&` are escaped** to `\u003c`, `\u003e`, `\u0026`. An issue
//!   body or a PR description is Markdown a person wrote, so this fires
//!   constantly — and `serde_json` on its own emits those three bytes
//!   literally.
//! - **A nil slice is `null`, not `[]`.** `splitCSV(" , , ")` returns nil, and
//!   `body["labels"] = splitCSV(...)` stores it, so the request carries
//!   `"labels":null`. See [`Body::set_csv`].
//!
//! And one comes from the *conditions*: a field is written only when it is
//! non-empty or true, so `create_pull` with `draft: false` omits the key rather
//! than sending `false`, and `update_issue` with nothing set sends `{}` —
//! which still counts as "there is a body" and therefore still sends the
//! `Content-Type` header.

use std::collections::BTreeMap;

use serde_json::Value;

use super::client::split_csv;

/// A Go `map[string]any` under construction, keyed for `json.Marshal`'s order.
#[derive(Default)]
pub struct Body(BTreeMap<String, Value>);

impl Body {
    /// `body := map[string]any{}`.
    pub fn new() -> Self {
        Self::default()
    }

    /// An unconditional key — `map[string]any{"title": p.Title}`.
    pub fn set(&mut self, key: &str, value: impl Into<Value>) {
        self.0.insert(key.to_string(), value.into());
    }

    /// `if p.Body != "" { body["body"] = p.Body }`.
    pub fn set_if_non_empty(&mut self, key: &str, value: &str) {
        if !value.is_empty() {
            self.set(key, value);
        }
    }

    /// `if p.Draft { body["draft"] = true }` — note the value is the literal
    /// `true`, so a false flag leaves no key at all.
    pub fn set_if_true(&mut self, key: &str, value: bool) {
        if value {
            self.set(key, true);
        }
    }

    /// `if p.Labels != "" { body["labels"] = splitCSV(p.Labels) }`.
    ///
    /// Two conditions, not one, and they disagree on exactly one input: a
    /// string that is **non-empty but all separators**. `" , , "` passes the
    /// gate and splits to a nil slice, so the key is present with the value
    /// `null`. `[]` would be a different request.
    pub fn set_csv(&mut self, key: &str, value: &str) {
        if value.is_empty() {
            return;
        }
        let parts = split_csv(value);
        if parts.is_empty() {
            self.set(key, Value::Null);
        } else {
            self.set(key, parts);
        }
    }

    /// The bytes `json.Marshal` would produce.
    ///
    /// Infallible in practice — every value here is a string, a bool, a list of
    /// strings or a map of strings — but the encoder's signature is fallible,
    /// and its `Err` becomes the tool error the model reads.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        crate::native::gojson::to_vec_marshal(&self.0)
            .map_err(|e| format!("marshaling request body: {e}"))
    }
}

/// `json.Unmarshal([]byte(p.Inputs), &map[string]string)`, error text included.
///
/// `trigger_workflow` is the only tool that parses a *caller-supplied* JSON
/// document, and Go's failure message reaches the model through
/// `fmt.Errorf("parsing workflow inputs (must be a JSON object): %w", err)` —
/// so `encoding/json`'s own wording is part of the interface.
///
/// Reproduced exactly for every **well-formed** document:
///
/// | input | answer |
/// |---|---|
/// | `{"a":"b"}` | the map |
/// | `{}` | an empty map, which marshals as `{}` |
/// | `null` | a **nil map**, no error — it marshals back as `null` |
/// | `{"a":null}` | `{"a":""}`, because a JSON null is Go's zero value |
/// | `[…]` / `5` / `"s"` / `true` | `cannot unmarshal <kind> into Go value of type map[string]string` |
/// | `{"a":1}` | `cannot unmarshal <kind> into Go value of type string` |
///
/// **Two divergences, both on malformed input, both deliberate:**
///
/// 1. A **syntax** error. Go's scanner has its own vocabulary
///    (`invalid character 'o' in literal null (expecting 'u')`) with no
///    `serde_json` equivalent, and porting `encoding/json`'s scanner to
///    reproduce a message would be a large amount of code for a string. The
///    one exception is a *truncated* document, whose wording is short enough to
///    reproduce and common enough to be worth it:
///    `unexpected end of JSON input`. `github_vectors.json` pins both cases,
///    the divergent one through its `rust_text` field.
/// 2. A document with **several** badly-typed values reports the kind of
///    whichever comes first, and "first" is document order in Go and sorted
///    order here (`serde_json::Map` is a `BTreeMap` — deliberately, since
///    that is what gives [`crate::native::gojson`] Go's map ordering). Only the
///    *kind word* can differ, and only for an input with two wrong values of
///    different types.
pub fn parse_string_map(raw: &str) -> Result<Option<BTreeMap<String, String>>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|e| {
        if e.classify() == serde_json::error::Category::Eof {
            "unexpected end of JSON input".to_string()
        } else {
            e.to_string()
        }
    })?;

    let object = match value {
        // `json.Unmarshal` of a literal null into a map leaves the map nil and
        // returns no error. The nil-ness survives: the caller stores it and
        // `json.Marshal` writes `"inputs":null`.
        Value::Null => return Ok(None),
        Value::Object(object) => object,
        other => {
            return Err(format!(
                "json: cannot unmarshal {} into Go value of type map[string]string",
                go_kind(&other)
            ))
        }
    };

    let mut out = BTreeMap::new();
    for (key, value) in object {
        let text = match value {
            Value::String(text) => text,
            // A JSON null decodes into a Go string as the zero value.
            Value::Null => String::new(),
            other => {
                return Err(format!(
                    "json: cannot unmarshal {} into Go value of type string",
                    go_kind(&other)
                ))
            }
        };
        out.insert(key, text);
    }
    Ok(Some(out))
}

/// The word `encoding/json` uses for a JSON value's kind in an unmarshal error.
///
/// `bool` rather than `boolean`, and one `number` for every numeric shape —
/// these are `decodeState`'s literals, not JSON Schema's type names.
fn go_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(body: &Body) -> String {
        String::from_utf8(body.encode().expect("encode")).expect("utf-8")
    }

    /// Sorted keys and HTML escaping, which is the whole of `json.Marshal` over
    /// a Go map — and the reason `create_issue`'s body is not in field order.
    #[test]
    fn a_body_is_sorted_and_html_escaped() {
        let mut body = Body::new();
        body.set("title", "Found a bug");
        body.set_if_non_empty("body", "It <broke> & burned");
        body.set_csv("labels", "bug, help wanted ,");
        assert_eq!(
            encoded(&body),
            concat!(
                r#"{"body":"It \u003cbroke\u003e \u0026 burned","#,
                r#""labels":["bug","help wanted"],"title":"Found a bug"}"#
            )
        );
    }

    /// The three conditions, each at the input that distinguishes it.
    #[test]
    fn a_key_appears_only_when_go_would_write_it() {
        let mut body = Body::new();
        body.set_if_non_empty("body", "");
        body.set_if_true("draft", false);
        body.set_csv("labels", "");
        assert_eq!(encoded(&body), "{}", "an empty update is still a body");

        let mut body = Body::new();
        body.set_if_true("draft", true);
        // Non-empty but all separators: the gate passes and the split is nil.
        body.set_csv("labels", " , , ");
        assert_eq!(encoded(&body), r#"{"draft":true,"labels":null}"#);
    }

    #[test]
    fn well_formed_inputs_parse_the_way_go_parses_them() {
        let map = |pairs: &[(&str, &str)]| {
            Some(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<BTreeMap<_, _>>(),
            )
        };
        assert_eq!(
            parse_string_map(r#"{"zebra":"z","alpha":"a"}"#),
            Ok(map(&[("alpha", "a"), ("zebra", "z")]))
        );
        assert_eq!(parse_string_map("{}"), Ok(map(&[])));
        // A literal null is a nil map, not an error and not an empty map — and
        // the difference is visible in the request body.
        assert_eq!(parse_string_map("null"), Ok(None));
        assert_eq!(parse_string_map(r#"{"a":null}"#), Ok(map(&[("a", "")])));
    }

    #[test]
    fn a_wrong_shape_is_gos_sentence() {
        for (input, kind) in [
            (r#"["a","b"]"#, "array"),
            ("[]", "array"),
            ("5", "number"),
            (r#""s""#, "string"),
            ("true", "bool"),
        ] {
            assert_eq!(
                parse_string_map(input),
                Err(format!(
                    "json: cannot unmarshal {kind} into Go value of type map[string]string"
                )),
                "{input}"
            );
        }
        for (input, kind) in [
            (r#"{"a":1}"#, "number"),
            (r#"{"a":true}"#, "bool"),
            (r#"{"a":{"b":"c"}}"#, "object"),
            (r#"{"a":[]}"#, "array"),
        ] {
            assert_eq!(
                parse_string_map(input),
                Err(format!(
                    "json: cannot unmarshal {kind} into Go value of type string"
                )),
                "{input}"
            );
        }
    }

    /// The one syntax error worth reproducing: a truncated document says the
    /// same thing in both languages, and it is the failure a hand-typed
    /// argument actually produces.
    #[test]
    fn a_truncated_document_is_gos_sentence_too() {
        for input in [r#"{"a":"#, "{", "[", r#"{"a""#] {
            assert_eq!(
                parse_string_map(input),
                Err("unexpected end of JSON input".to_string()),
                "{input}"
            );
        }
    }
}
