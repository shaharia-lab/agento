//! One session's transcript → one `session_search` document (#435).
//!
//! This is the routing half of the indexer: which of a decoded event's text
//! belongs in `user_text`, which in `assistant_text`, and which in `tool_text`.
//! The *shaping* half — markdown and tag removal, whitespace collapsing and the
//! three caps — is `search::normalize`'s (#434), and nothing here duplicates it.
//!
//! # It rides the read the processors already do
//!
//! The worker re-reads a changed session's transcript to recompute its nine
//! insight passes, and that read is where every byte this module wants already
//! is. So [`DocAccumulator`] is threaded into `processors::feed` and observes
//! the same `Event` stream the processors see, rather than opening the file a
//! second time. **Indexing therefore costs no additional file I/O**, which is
//! the property the issue is built on: index freshness equals insight freshness
//! for free.
//!
//! It also means the `file-history-snapshot` skip is inherited — `feed` drops
//! those before anything observes them — and that sub-agent transcripts are
//! indexed with the parent, additively, exactly as their tool calls and cost
//! already are.
//!
//! # Routing is by content shape, never by `isSidechain`
//!
//! A `user` event is one of three things and only the first is a person typing:
//!
//! - a genuine turn ([`transcript::is_user_turn_content`]) → `user_text`;
//! - a **carrier for `tool_result` blocks**, which is how a tool's output
//!   reaches the transcript at all → `tool_text`;
//! - **injected machinery** — `<command-name>`, a system reminder, an interrupt
//!   marker — which is text no one wrote and no one will search for → dropped.
//!
//! `is_user_turn_content` answers `false` to the second *and* the third, so the
//! `tool_result` check has to come first and be its own; using the predicate
//! alone would file every tool result under "injected" and lose the whole
//! column. `a_tool_result_carrier_is_indexed_as_tool_text_not_dropped` is that
//! distinction.
//!
//! Deliberately **not** `is_turn_start`, which also requires `!is_sidechain`: a
//! sub-agent's own transcript has the flag set on every event, so that predicate
//! would route a delegated run's entire conversation into `tool_text`. The flag
//! answers "was this delegated", which is a different question from "who wrote
//! this".
//!
//! # What a tool contributes
//!
//! A `tool_use` block contributes its **name and the string leaves of its
//! input** — a `Bash` command, a `Read` path, a `Grep` pattern, a `Task`
//! prompt. Those are the things people remember and search for; the numbers and
//! booleans beside them are noise that would only dilute the ranking.
//!
//! This is the one place a `tool_use` `input` is read through a
//! `serde_json::Value`, and it is sound *here* precisely because nothing is
//! re-encoded: the standing rule (`chats.rs`, `sessions/detail.rs`) is that a
//! `Value` round trip sorts keys and respells numbers, which matters when the
//! bytes go back on the wire. These bytes become tokens in an index and never
//! travel anywhere, so the ordering is unobservable — and the raw form has no
//! way to enumerate leaves without parsing it anyway.

use serde_json::Value;

use super::transcript::{self, ContentBlock, Event};
use crate::native::search::{self, normalize};

/// One session's three text columns, accumulated event by event.
///
/// A thin router over [`normalize::DocBuilder`], which owns the caps and the
/// shared session budget. Constructing one costs nothing, so the worker builds
/// one per session unconditionally.
#[derive(Debug, Clone, Default)]
pub struct DocAccumulator {
    builder: normalize::DocBuilder,
}

impl DocAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Route one decoded event's text into the right column.
    ///
    /// Called for every event of the parent transcript and of each sub-agent's,
    /// in file order, from `processors::feed`.
    pub fn observe(&mut self, ev: &Event) {
        // The session budget is spent. Every `push` below would return
        // immediately anyway; returning here is what stops a 400-message session
        // parsing content blocks it can no longer index anything from.
        if self.builder.remaining() == 0 {
            return;
        }
        let Some(message) = ev.message.as_ref() else {
            return;
        };

        match ev.event_type.as_str() {
            "user" => self.observe_user(&message.content),
            "assistant" => self.observe_assistant(&message.content),
            // `summary`, `system`, and anything Claude Code adds later. None of
            // it is conversation, and an unknown event type contributing to the
            // index is how a format change silently becomes search noise.
            _ => {}
        }
    }

    fn observe_user(&mut self, content: &Value) {
        let mut carried_a_result = false;
        for payload in tool_result_payloads(content) {
            carried_a_result = true;
            self.builder
                .push_tool(&transcript::extract_text_content(payload));
        }
        if carried_a_result {
            return;
        }
        // `is_injected_user_content`, not `is_user_turn_content`. The latter is
        // "no `tool_result` block **and** not injected", and the early return
        // above has already settled the first half — so calling it here would
        // re-run `parse_content_blocks` over the same array to re-derive an
        // answer we have, and that call clones every block's `Value`.
        if !transcript::is_injected_user_content(content) {
            self.builder
                .push_user(&transcript::extract_text_content(content));
        }
    }

    fn observe_assistant(&mut self, content: &Value) {
        for block in transcript::parse_content_blocks(content) {
            match block.block_type.as_str() {
                "text" => self.builder.push_assistant(&block.text),
                // `thinking` is deliberately absent: current models redact it,
                // so the field is an empty string plus a signature, and indexing
                // it would add a column's worth of nothing.
                "tool_use" => self.builder.push_tool(&tool_use_text(&block)),
                _ => {}
            }
        }
    }

    /// Finish the document's three text columns, under the key it belongs to.
    ///
    /// **The `title` column is left empty**, and that is not an omission: the
    /// title comes from the *cache row*, not from the transcript, so it is the
    /// only column this accumulator cannot produce from what it saw. The writer
    /// fills it with [`normalize_title`] inside the transaction that stores the
    /// row, which is also what keeps the indexed title consistent with the cache
    /// row the same transaction is reconciled against.
    pub fn into_doc(self, session_id: &str, project_path: &str) -> search::SearchDoc {
        let (user_text, assistant_text, tool_text) = self.builder.into_parts();
        search::SearchDoc {
            session_id: session_id.to_string(),
            project_path: project_path.to_string(),
            title: String::new(),
            user_text,
            assistant_text,
            tool_text,
        }
    }
}

/// The `title` column, shaped like every other one.
///
/// A `custom_title` is text the user typed and a `preview` is the first 120
/// characters of a prompt, so both can carry markdown, tags and runs of
/// whitespace exactly as a message can. Capped at [`normalize::MESSAGE_CAP`],
/// which no real title approaches — the cap is there so the column cannot become
/// the one unbounded thing in a bounded row.
///
/// Deliberately outside the session budget: the title is the highest-weighted
/// column in the ranking (8×), and a session whose conversation exhausted the
/// budget would otherwise be indexed under no title at all.
pub fn normalize_title(raw: &str) -> String {
    normalize::normalize_text(raw, normalize::MESSAGE_CAP)
}

/// Each `tool_result` block's payload, borrowed straight out of the message's
/// content array.
///
/// **Deliberately not routed through [`transcript::ContentBlock`]**, which is
/// the obvious way to write this and is the expensive one. That struct does not
/// decode a `tool_result`'s `content`, and adding the field to it would reach
/// far past this module: `parse_content_blocks` is called four times per message
/// by the processors, and `parse_content_blocks_raw` is on the
/// `GET /api/claude-sessions/{id}` read path — where the payload is currently
/// skipped by the decoder entirely. A `Read` of a large file is a multi-megabyte
/// `tool_result`, so adding the field would make every one of those callers
/// materialize and retain payloads none of them look at, to serve one reader
/// here.
///
/// Borrowing costs nothing instead: the content is already a `Value` on
/// `Message`, so this only walks it. The block shape is checked by hand for the
/// same reason — one `type` comparison against a shared type's whole decode.
fn tool_result_payloads(content: &Value) -> impl Iterator<Item = &Value> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        // The payload a tool's output arrives in. Absent is `Value::Null`, which
        // `extract_text_content` answers `""` to, so a malformed block
        // contributes nothing rather than needing its own arm.
        .map(|block| block.get("content").unwrap_or(&Value::Null))
}

/// A `tool_use` block as text: the tool's name, then the string leaves of its
/// input.
///
/// The name is included because it is a search term in its own right — someone
/// looking for the session where they ran a `WebFetch` has nothing else to go
/// on.
fn tool_use_text(block: &ContentBlock) -> String {
    let mut out = String::with_capacity(block.name.len() + 64);
    out.push_str(&block.name);
    let Some(raw) = block.input.as_ref() else {
        return out;
    };
    // Undecodable input contributes its name and nothing else, which is the
    // same direction every rule in `normalize` fails in: keep what is certainly
    // text, drop what cannot be established.
    let Ok(value) = serde_json::from_str::<Value>(raw.get()) else {
        return out;
    };
    push_string_leaves(&value, &mut out);
    out
}

/// Append every string leaf of `value`, in document order, space separated.
///
/// **Bounded by [`normalize::TOOL_RESULT_CAP`]**, which is the cap the result
/// will be truncated to anyway: building a multi-megabyte `String` so that
/// `push_tool` can keep two kilobytes of it is the shape #434's own "every
/// forward scan needs a bound" note is about.
///
/// The bound is applied **inside a leaf as well as between leaves**, and that is
/// the half worth stating: the case it exists for is a `Write` call, whose input
/// is `{"file_path": "…", "content": "<the whole file>"}` — *one* enormous leaf.
/// A guard that only checked between leaves would leave the dominant case
/// completely uncovered while looking as though it handled it. The many-small-
/// leaves shape is real too, so both are tested.
fn push_string_leaves(value: &Value, out: &mut String) {
    if out.len() >= normalize::TOOL_RESULT_CAP {
        return;
    }
    match value {
        Value::String(s) => {
            if !s.is_empty() {
                out.push(' ');
                out.push_str(truncate_on_boundary(
                    s,
                    normalize::TOOL_RESULT_CAP.saturating_sub(out.len()),
                ));
            }
        }
        Value::Array(items) => {
            for item in items {
                push_string_leaves(item, out);
            }
        }
        // Keys are dropped and only values kept: an argument name is schema, not
        // content, and `file_path` as an indexed token would match every session
        // that ever read a file.
        Value::Object(fields) => {
            for item in fields.values() {
                push_string_leaves(item, out);
            }
        }
        // Numbers, booleans and nulls are not search terms.
        _ => {}
    }
}

/// `s` cut to at most `max` bytes, never through a character.
///
/// `normalize_text` applies its own cap afterwards, so this is only about not
/// building the huge intermediate — but it still must not split a codepoint,
/// because slicing a `str` mid-character panics.
fn truncate_on_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(event_type: &str, content: Value) -> Event {
        Event {
            event_type: event_type.to_string(),
            message: Some(transcript::Message {
                role: event_type.to_string(),
                content,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// `(user_text, assistant_text, tool_text)` after observing `events`.
    fn parts(events: Vec<Event>) -> (String, String, String) {
        let mut doc = DocAccumulator::new();
        for ev in &events {
            doc.observe(ev);
        }
        doc.builder.into_parts()
    }

    #[test]
    fn a_user_turn_and_an_assistant_reply_land_in_their_own_columns() {
        let (user, assistant, tool) = parts(vec![
            event("user", json!("how do I fix the auth bug")),
            event(
                "assistant",
                json!([{"type": "text", "text": "check the token scope"}]),
            ),
        ]);

        assert_eq!(user, "how do I fix the auth bug");
        assert_eq!(assistant, "check the token scope");
        assert_eq!(tool, "");
    }

    /// The distinction the module header is about: a `tool_result` carrier and
    /// injected machinery are both "not a user turn", and they must not have the
    /// same fate.
    ///
    /// Routing on `is_user_turn_content` alone drops both, which empties
    /// `tool_text` of every real tool result — the column would still exist,
    /// still be populated by `tool_use` blocks, and simply never contain an
    /// error message. That is the failure this asserts against.
    #[test]
    fn a_tool_result_carrier_is_indexed_as_tool_text_not_dropped() {
        let (user, _assistant, tool) = parts(vec![event(
            "user",
            json!([{
                "type": "tool_result",
                "tool_use_id": "t1",
                "content": "error: connection refused",
            }]),
        )]);

        assert_eq!(tool, "error: connection refused");
        assert_eq!(user, "", "a carrier is not something a person typed");
    }

    /// A tool result whose content is an array of blocks, which is the other
    /// shape the format allows.
    #[test]
    fn a_tool_results_array_content_is_indexed_too() {
        let (_user, _assistant, tool) = parts(vec![event(
            "user",
            json!([{
                "type": "tool_result",
                "tool_use_id": "t1",
                "content": [{"type": "text", "text": "panic at line 12"}],
            }]),
        )]);

        assert_eq!(tool, "panic at line 12");
    }

    /// Injected content is machinery no one wrote, and it is dropped from every
    /// column rather than being filed under one of them.
    #[test]
    fn injected_user_content_is_not_indexed() {
        let (user, assistant, tool) = parts(vec![event(
            "user",
            json!("<command-name>/compact</command-name>"),
        )]);

        assert_eq!(
            (user.as_str(), assistant.as_str(), tool.as_str()),
            ("", "", "")
        );
    }

    /// A tool call contributes its name and the strings inside its input, and
    /// nothing else.
    #[test]
    fn a_tool_call_contributes_its_name_and_its_string_arguments() {
        let (_user, _assistant, tool) = parts(vec![event(
            "assistant",
            json!([{
                "type": "tool_use",
                "id": "t1",
                "name": "Bash",
                "input": {"command": "cargo test --release", "timeout": 120, "quiet": false},
            }]),
        )]);

        assert_eq!(tool, "Bash cargo test --release");
    }

    /// Object *keys* are schema and are dropped; only the values are content.
    ///
    /// Indexing `file_path` as a token would make every session that read a file
    /// a hit for it.
    #[test]
    fn argument_names_are_not_indexed() {
        let (_user, _assistant, tool) = parts(vec![event(
            "assistant",
            json!([{
                "type": "tool_use",
                "name": "Read",
                "input": {"file_path": "/src/auth.rs"},
            }]),
        )]);

        assert_eq!(tool, "Read /src/auth.rs");
    }

    /// Nested and array-valued arguments still contribute their strings.
    #[test]
    fn string_leaves_are_found_at_any_depth() {
        let (_user, _assistant, tool) = parts(vec![event(
            "assistant",
            json!([{
                "type": "tool_use",
                "name": "Edit",
                "input": {"edits": [{"old": "alpha"}, {"old": "beta"}]},
            }]),
        )]);

        assert_eq!(tool, "Edit alpha beta");
    }

    /// Thinking is redacted by current models, so the block carries no text —
    /// and it is not indexed even when it does.
    #[test]
    fn thinking_is_not_indexed() {
        let (_user, assistant, tool) = parts(vec![event(
            "assistant",
            json!([{"type": "thinking", "thinking": "let me consider the options"}]),
        )]);

        assert_eq!((assistant.as_str(), tool.as_str()), ("", ""));
    }

    /// The case the bound actually exists for: **one** enormous leaf, which is
    /// what a `Write` call's `content` argument is.
    ///
    /// The many-small-leaves test below caught a guard checked only *between*
    /// leaves; this one does not, and it is the commoner shape by far. A 4 MiB
    /// file uploaded through `Write` is 4 MiB of `String` built to keep 2 KiB.
    #[test]
    fn a_single_enormous_leaf_is_truncated_rather_than_copied_whole() {
        let file = "x".repeat(4 * 1024 * 1024);
        let block = ContentBlock {
            block_type: "tool_use".into(),
            name: "Write".into(),
            input: Some(
                serde_json::value::RawValue::from_string(
                    json!({"file_path": "/a.txt", "content": file}).to_string(),
                )
                .expect("raw"),
            ),
            ..Default::default()
        };

        let text = tool_use_text(&block);

        assert!(
            text.len() <= normalize::TOOL_RESULT_CAP + 16,
            "collected {} bytes from a 4 MiB single-leaf input",
            text.len(),
        );
        assert!(text.starts_with("Write "), "got {:?}", &text[..16]);
        // `serde_json` sorts an object's keys, so `content` is walked before
        // `file_path` and eats the whole budget — the path does not make it in.
        // That is unchanged from building the string in full and letting
        // `normalize_text` trim it, which cut at the same place; the only thing
        // this fix changes is how much is built to get there.
        assert!(text.contains('x'));
    }

    /// Truncating a leaf must not split a character — slicing a `str`
    /// mid-codepoint panics, and a tool argument is arbitrary user text.
    #[test]
    fn truncating_a_leaf_never_splits_a_character() {
        // Multi-byte throughout, so almost every byte offset is a split point.
        let text = "é".repeat(normalize::TOOL_RESULT_CAP);
        for max in [0, 1, 2, 3, 1_023, normalize::TOOL_RESULT_CAP - 1] {
            let cut = truncate_on_boundary(&text, max);
            assert!(cut.len() <= max, "{} > {max}", cut.len());
            assert!(text.starts_with(cut));
        }
    }

    /// The bound on a tool call's input, which is what stops a `Write` of a
    /// large file materializing in full before the cap trims it.
    #[test]
    fn a_huge_tool_input_is_bounded_rather_than_materialized_whole() {
        let leaf = "x".repeat(1_024);
        let input: Vec<Value> = (0..64).map(|_| json!(leaf)).collect();
        let block = ContentBlock {
            block_type: "tool_use".into(),
            name: "Write".into(),
            input: Some(
                serde_json::value::RawValue::from_string(json!(input).to_string()).expect("raw"),
            ),
            ..Default::default()
        };

        let text = tool_use_text(&block);

        assert!(
            text.len() < normalize::TOOL_RESULT_CAP + leaf.len() + 8,
            "collected {} bytes from a 64 KiB input",
            text.len()
        );
    }

    /// [`push_string_leaves`] recurses, so the depth it can reach has to be
    /// bounded by something.
    ///
    /// It is, and by the parse rather than by a check here: `serde_json`'s
    /// deserializer enforces a 128-level recursion limit, so a `Value` this
    /// function is handed can never be deeper than that. Input past the limit
    /// fails to parse and takes the "name only" arm — which is the same
    /// keep-what-is-certain direction every rule in `normalize` fails in.
    ///
    /// Pinned rather than reasoned about, because the guarantee lives in a
    /// dependency's default and a `Value` built in memory would not have it.
    #[test]
    fn a_deeply_nested_tool_input_is_bounded_by_the_parser() {
        let deep = format!("{}\"x\"{}", "[".repeat(600), "]".repeat(600));
        let block = ContentBlock {
            block_type: "tool_use".into(),
            name: "Deep".into(),
            input: Some(serde_json::value::RawValue::from_string(deep).expect("raw")),
            ..Default::default()
        };

        assert_eq!(
            tool_use_text(&block),
            "Deep",
            "input past serde_json's recursion limit must not parse, and must \
             not recurse",
        );

        // …and a depth the parser *does* accept is walked normally, so the
        // assertion above is about the limit rather than about all nesting.
        let shallow = format!("{}\"found\"{}", "[".repeat(20), "]".repeat(20));
        let block = ContentBlock {
            block_type: "tool_use".into(),
            name: "Deep".into(),
            input: Some(serde_json::value::RawValue::from_string(shallow).expect("raw")),
            ..Default::default()
        };
        assert_eq!(tool_use_text(&block), "Deep found");
    }

    /// An event with no message at all — a `file-history-snapshot` reaching here
    /// would be one, though `feed` already drops those — contributes nothing and
    /// does not panic.
    #[test]
    fn an_event_without_a_message_is_ignored() {
        let (user, assistant, tool) = parts(vec![Event {
            event_type: "user".into(),
            ..Default::default()
        }]);

        assert_eq!(
            (user.as_str(), assistant.as_str(), tool.as_str()),
            ("", "", "")
        );
    }

    /// The key columns are carried and the title is left for the writer, which
    /// is the only column that does not come from the transcript.
    #[test]
    fn the_key_columns_are_carried_and_the_title_is_the_writers() {
        let mut doc = DocAccumulator::new();
        doc.observe(&event("user", json!("hello")));

        let built = doc.into_doc("s1", "/a");

        assert_eq!(built.session_id, "s1");
        assert_eq!(built.project_path, "/a");
        assert_eq!(built.user_text, "hello");
        assert_eq!(built.title, "", "the title comes from the cache row");
    }

    /// A title is shaped like every other column — a user's rename can hold
    /// markdown and newlines just as a message can.
    #[test]
    fn a_title_is_normalized() {
        assert_eq!(normalize_title("the  **auth**\nfix"), "the **auth** fix");
        assert_eq!(normalize_title(""), "");
    }

    /// The whole-session budget is shared across the three columns, so a session
    /// that is all tool output cannot cost more than one that is all
    /// conversation. Asserted through the router rather than only in
    /// `normalize`, because the router is what decides how many pushes happen.
    #[test]
    fn the_session_budget_bounds_every_column_together() {
        let big = "word ".repeat(4 * 1024);
        let events: Vec<Event> = (0..400)
            .map(|_| {
                event(
                    "user",
                    json!([{"type": "tool_result", "tool_use_id": "t", "content": big}]),
                )
            })
            .collect();

        let (user, assistant, tool) = parts(events);

        assert!(
            user.len() + assistant.len() + tool.len() <= normalize::SESSION_CAP,
            "indexed {} bytes for one session",
            user.len() + assistant.len() + tool.len()
        );
        assert!(
            !tool.is_empty(),
            "the cap must bound, not empty, the column"
        );
    }
}
