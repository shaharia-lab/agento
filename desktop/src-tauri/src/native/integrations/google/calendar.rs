//! The `calendar` service's two tools, ported from
//! `internal/integrations/google/calendar.go`.
//!
//! Conventions are `native/integrations/github/repos.rs`'s. What is different is
//! that the request is built by a **generated client** rather than by the handler
//! — so every URL, query key and body field below was measured off
//! `google.golang.org/api/calendar/v3` rather than read from `calendar.go`. See
//! `super::client` for what that does and does not let the port reproduce.
//!
//! Two things the generated client contributes that the handler never mentions:
//!
//! - **`alt=json&prettyPrint=false` on every call**, sorted in with everything
//!   else by `url.Values.Encode`.
//! - **`omitempty` on the request body.** `calendar.Event`'s fields carry it, so
//!   an empty `description` sends *no key* — measured both ways.
//!
//! And one the handler does: **`time_min` defaults to "now"**, so the request is
//! not deterministic. The vectors record that case with the value redacted and
//! assert the shape instead; see `tests_vectors`.

use schemars::JsonSchema;
use serde_json::{json, Value};

use crate::claude::{new_tool, CancellationToken, ToolDef};
use crate::native::gourl::Values;

use super::client::{Api, Client};
use super::text_result;

/// The two parameters every generated call carries.
pub(super) fn base_query() -> Values {
    let mut query = Values::new();
    query.set("alt", "json");
    query.set("prettyPrint", "false");
    query
}

/// `create_event`.
#[allow(dead_code)] // read through serde, never constructed in Rust
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateEventInput {
    /// required,The title of the event
    summary: String,
    /// required,Start time in RFC3339 format (e.g. 2026-03-01T10:00:00-07:00)
    start: String,
    /// required,End time in RFC3339 format
    end: String,
    /// Optional description of the event
    description: String,
}

pub fn create_event(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "create_event",
        "Creates a new event on the user's primary Google Calendar.",
        move |input: CreateEventInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // `calendar.Event` with `omitempty` everywhere: an empty
                // description sends no key at all, and `TimeZone` is the
                // handler's literal `"UTC"` on both ends.
                let mut event = json!({
                    "end": {"dateTime": input.end, "timeZone": "UTC"},
                    "start": {"dateTime": input.start, "timeZone": "UTC"},
                    "summary": input.summary,
                });
                if !input.description.is_empty() {
                    event["description"] = Value::String(input.description.clone());
                }

                let body = super::marshal(&event)?;
                // The decode is inside the *same* `map_err` as the request,
                // because in Go both are `Do()` — a response the client cannot
                // decode surfaces under the handler's own sentence, not a bare
                // one. That holds at every decode site in this module.
                let created: Event = client
                    .post_json(
                        &ct,
                        Api::Calendar,
                        "calendars/primary/events",
                        &base_query(),
                        body,
                    )
                    .await
                    .and_then(|raw| super::decode(&raw))
                    // The result reads fields off the **response**, not the
                    // request, so a server that renamed the event is what the
                    // model is told.
                    .map_err(|e| format!("creating calendar event: {e}"))?;
                Ok(text_result(format!(
                    "Event created: {}\nID: {}\nLink: {}",
                    created.summary, created.id, created.html_link
                )))
            }
        },
    )
}

/// `view_events`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewEventsInput {
    /// Lower bound for event end time in RFC3339 format. Defaults to now.
    time_min: String,
    /// Upper bound for event start time in RFC3339 format.
    time_max: String,
    /// Maximum number of events to return (default 10, max 100)
    max_results: i64,
}

pub fn view_events(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "view_events",
        "Lists events from the user's primary Google Calendar within an optional time range.",
        move |input: ViewEventsInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let max_results = if input.max_results <= 0 || input.max_results > 100 {
                    10
                } else {
                    input.max_results
                };

                let mut query = base_query();
                query.set("maxResults", max_results.to_string());
                query.set("orderBy", "startTime");
                query.set("singleEvents", "true");
                // `time.Now().UTC().Format(time.RFC3339)` — seconds precision, a
                // literal `Z`, and the reason one vector redacts this value.
                query.set(
                    "timeMin",
                    if input.time_min.is_empty() {
                        super::now_rfc3339()
                    } else {
                        input.time_min.clone()
                    },
                );
                if !input.time_max.is_empty() {
                    query.set("timeMax", &input.time_max);
                }

                let listed: EventList = client
                    .get(&ct, Api::Calendar, "calendars/primary/events", &query)
                    .await
                    .and_then(|raw| super::decode(&raw))
                    .map_err(|e| format!("listing calendar events: {e}"))?;
                if listed.items.is_empty() {
                    return Ok(text_result(
                        "No events found in the specified range.".to_string(),
                    ));
                }

                let mut out = format!("Found {} event(s):\n", listed.items.len());
                for event in listed.items() {
                    // `ev.Start.DateTime` falls back to `.Date` for an all-day
                    // event — and a missing `start` object is an empty string
                    // rather than a panic, which is the one place this is
                    // gentler than Go. See the module docs on `read_email`.
                    let start = event
                        .start
                        .as_ref()
                        .map(|when| {
                            if when.date_time.is_empty() {
                                when.date.clone()
                            } else {
                                when.date_time.clone()
                            }
                        })
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "\n- {}\n  Start: {}\n  ID: {}\n  Link: {}\n",
                        event.summary, start, event.id, event.html_link
                    ));
                }
                Ok(text_result(out))
            }
        },
    )
}

/// The fields of `calendar.Event` the two handlers read.
///
/// Every one carries the three decode rules `desktop/CLAUDE.md` sets out — a
/// JSON `null` is a zero value, an array is not a struct, and a bare `null` is a
/// no-op — because this is Google's response and nothing constrains its shape.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct Event {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    id: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    summary: String,
    #[serde(rename = "htmlLink")]
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    html_link: String,
    start: Option<crate::native::gojson::GoStruct<EventDateTime>>,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct EventDateTime {
    #[serde(rename = "dateTime")]
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    date_time: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    date: String,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct EventList {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    items: Vec<crate::native::gojson::GoStruct<Event>>,
}

impl EventList {
    fn items(&self) -> impl Iterator<Item = &Event> {
        self.items.iter().map(|wrapped| &wrapped.0)
    }
}
