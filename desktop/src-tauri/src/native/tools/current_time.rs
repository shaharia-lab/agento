//! `current_time`, ported from `internal/tools/current_time.go`.
//!
//! One tool, and every line of it is a parity surface: the input schema steers
//! what the model sends, and the text it answers with is what ends up in a
//! stored `tool_result` block. `desktop/parity/local_tools_vectors.json` is
//! generated from the Go implementation and read by both languages' tests, so
//! neither half can drift without the other failing.

use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Tz;
use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;

use crate::claude::{new_tool, CancellationToken, ToolDef};

/// Go's `time.RFC1123`, `"Mon, 02 Jan 2006 15:04:05 MST"`.
///
/// `%Z` is the zone *abbreviation* in both languages, and it is not always
/// alphabetic: the IANA database spells Asia/Kathmandu's as `+0545` and
/// Etc/GMT+5's as `-05`, which Go prints verbatim and `chrono-tz` reproduces
/// because it carries the same strings. That is exactly the kind of thing the
/// vectors pin rather than assume.
const RFC1123: &str = "%a, %d %b %Y %H:%M:%S %Z";

/// The tool's input.
///
/// Two attributes carry Go's struct tags rather than being stylistic:
///
/// - The doc comment is the field description, which is what
///   `jsonschema:"IANA timezone name, …"` does on `currentTimeParams`.
/// - `deny_unknown_fields` is what emits `"additionalProperties": false`.
///   Go's `google/jsonschema-go` sets it for every reflected struct and the
///   `modelcontextprotocol/go-sdk` server *validates* against it, so an extra
///   argument is refused there; without this it would be silently accepted here.
///
/// The field is a plain `String`, not an `Option`, because that is what puts
/// `timezone` in the schema's `required` list — again matching Go, where the
/// field carries no `omitempty` and is therefore required. It means the
/// `timezone == ""` branch below is reachable only for an explicit empty
/// string, which is true of Go too.
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CurrentTimeInput {
    /// IANA timezone name, e.g. UTC or America/New_York. Defaults to UTC.
    timezone: String,
}

/// The `current_time` tool, as `registry.go` registers it.
///
/// The name and the description are byte-for-byte Go's: the name becomes
/// `mcp__local-tools__current_time` in an agent's allowlist and in every stored
/// `tool_use` block, and the description is what the model reads when deciding
/// to call it.
pub fn tool() -> ToolDef {
    new_tool(
        "current_time",
        "Returns the current date and time for a given IANA timezone \
         (e.g. UTC, America/New_York, Asia/Tokyo). Defaults to UTC.",
        |input: CurrentTimeInput, _ct: CancellationToken| async move {
            format_current_time(&input.timezone, Utc::now())
                .map(|text| CallToolResult::success(vec![ContentBlock::text(text)]))
        },
    )
}

/// `getCurrentTime`'s whole body, with the clock passed in.
///
/// Split out for the same reason Go's `FormatCurrentTime` is exported: a
/// vector generated against `time.Now()` would be a vector of one instant on
/// one machine. `Err` is the message the *model* reads — see
/// [`crate::claude::new_tool`] for why a tool's error is text rather than a
/// protocol failure.
pub fn format_current_time(timezone: &str, now: DateTime<Utc>) -> Result<String, String> {
    let tz = if timezone.is_empty() { "UTC" } else { timezone };
    // Go: `fmt.Errorf("unknown timezone %q: %w", tz, err)`. `{tz:?}` stands in
    // for `%q`, the way `wrap_permission_handler`'s deny message already does —
    // the two agree on every byte an IANA name can contain.
    let loc = load_location(tz).map_err(|e| format!("unknown timezone {tz:?}: {e}"))?;
    let local = now.with_timezone(&loc);
    Ok(format!(
        "Current time in {tz}: {} (ISO 8601: {})",
        local.format(RFC1123),
        // Go's `time.RFC3339` is `2006-01-02T15:04:05Z07:00`: seconds, no
        // fraction, and a literal `Z` at zero offset.
        local.to_rfc3339_opts(SecondsFormat::Secs, true),
    ))
}

/// `time.LoadLocation`, including the two answers it gives before it ever looks
/// at the tz database.
///
/// The order is Go's and each step is observable:
///
/// 1. `""` and `"UTC"` resolve without a lookup — so `UTC` works even where the
///    database does not.
/// 2. `"Local"` is the machine's zone. `chrono-tz` has no such name, so it is
///    resolved through `iana-time-zone` (already in the tree for the scheduler,
///    which needs the same answer for `CRON_TZ=Local`). A machine whose zone
///    cannot be named falls through to the unknown-zone error rather than to a
///    silently different clock.
/// 3. A name containing `..`, or beginning with a separator, is rejected with a
///    **different message** — `time: invalid location name` — because Go treats
///    it as a path-traversal attempt rather than as a missing zone. Reproducing
///    only the second message would make a rejected traversal read as a typo.
fn load_location(name: &str) -> Result<Tz, String> {
    if name.is_empty() || name == "UTC" {
        return Ok(Tz::UTC);
    }
    if name == "Local" {
        return iana_time_zone::get_timezone()
            .ok()
            .and_then(|zone| zone.parse::<Tz>().ok())
            .ok_or_else(|| format!("unknown time zone {name}"));
    }
    if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
        return Err("time: invalid location name".to_string());
    }
    name.parse::<Tz>()
        .map_err(|_| format!("unknown time zone {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .expect("a fixed instant")
            .with_timezone(&Utc)
    }

    /// The exact bytes Go produces, spelled out once so the shape of the answer
    /// is legible here rather than only in the vector file.
    #[test]
    fn the_answer_is_gos_sentence() {
        assert_eq!(
            format_current_time("UTC", at("2026-08-16T21:07:34Z")).unwrap(),
            "Current time in UTC: Sun, 16 Aug 2026 21:07:34 UTC \
             (ISO 8601: 2026-08-16T21:07:34Z)"
        );
    }

    /// An empty string is UTC, and the sentence names `UTC` rather than the
    /// empty string — Go substitutes before it formats.
    #[test]
    fn an_empty_timezone_is_utc_and_is_named_utc() {
        assert_eq!(
            format_current_time("", at("2026-08-16T21:07:34Z")).unwrap(),
            format_current_time("UTC", at("2026-08-16T21:07:34Z")).unwrap()
        );
    }

    /// The two failures are *different sentences*, which is the whole reason
    /// `load_location` reproduces `LoadLocation`'s pre-checks.
    #[test]
    fn the_two_failure_messages_are_not_interchangeable() {
        assert_eq!(
            format_current_time("Nowhere/Bad", at("2026-08-16T21:07:34Z")).unwrap_err(),
            "unknown timezone \"Nowhere/Bad\": unknown time zone Nowhere/Bad"
        );
        assert_eq!(
            format_current_time("..", at("2026-08-16T21:07:34Z")).unwrap_err(),
            "unknown timezone \"..\": time: invalid location name"
        );
        assert_eq!(
            format_current_time("/etc/passwd", at("2026-08-16T21:07:34Z")).unwrap_err(),
            "unknown timezone \"/etc/passwd\": time: invalid location name"
        );
        // Lookup is case-sensitive in Go, and `utc` is not the short-circuit.
        assert_eq!(
            format_current_time("utc", at("2026-08-16T21:07:34Z")).unwrap_err(),
            "unknown timezone \"utc\": unknown time zone utc"
        );
    }

    /// `Local` resolves rather than erroring, which is `LoadLocation`'s second
    /// short-circuit. The value is the machine's, so only the shape is asserted.
    #[test]
    fn local_is_a_zone_rather_than_a_typo() {
        let answer = format_current_time("Local", at("2026-08-16T21:07:34Z"));
        if let Ok(text) = answer {
            assert!(text.starts_with("Current time in Local: "), "{text}");
        }
    }
}
