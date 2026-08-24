//! Transcript text → indexable text. Pure functions, no I/O, no database.
//!
//! The indexer (#435) hands this the strings
//! `native/insights/transcript.rs`'s decoder already produced, and takes back
//! the three text columns [`super::SearchDoc`] carries. Everything here is a
//! deterministic function of its input — the `null`-tolerance rules
//! (`gojson::null_is_zero_value`) apply upstream, at the decode, never here.
//!
//! # This is deliberately not a markdown parser
//!
//! FTS5's `unicode61` tokenizer already treats every punctuation byte as a
//! separator, so `**bold**` tokenizes as `bold` and the phrase query
//! `"some bold text"` matches the source `some **bold** text` with nothing
//! stripped at all. Emphasis, headings, list markers, blockquotes, table pipes
//! and backticks therefore need no handling here: they cannot form a token.
//! `markers_are_not_tokens_so_a_phrase_matches_across_them` pins that against a
//! real FTS5 table rather than asserting it from the documentation.
//!
//! What is left is the three things tokenization does *not* solve:
//!
//! - **URL noise.** `[the auth guide](https://example.com/docs/v2/auth)` would
//!   otherwise index `example`, `com`, `docs`, `v2` and `auth` as content, so a
//!   search for `auth` ranks a session that merely *linked* somewhere alongside
//!   one that discussed it. Only a link's own destination is dropped; a **bare**
//!   URL in prose is text the user typed, and is kept.
//! - **Snippet readability.** `snippet()` returns the stored bytes, so the text
//!   a user sees in #438 is whatever is stored here. Collapsing whitespace and
//!   dropping tag and fence syntax is what makes that a sentence rather than a
//!   fragment of source.
//! - **Size.** One `Read` of a large file is a multi-megabyte tool result.
//!   Uncapped, a single session's index entry outweighs a thousand real ones and
//!   drowns them in the ranking.
//!
//! # Every rule removes syntax, never words
//!
//! That is the standing constraint, and the reason this module is small. Code
//! content and error strings are kept **verbatim** — people search for the exact
//! text of a panic — so a fence loses its ``` and its info string and keeps
//! every line between them. Where a rule could plausibly eat prose, the test is
//! written from the other side: `a_less_than_sign_in_prose_is_not_a_tag` and
//! `incomplete_link_syntax_keeps_every_word` exist to fail if it does.
//!
//! # Linear time is a property of the code, not an aspiration
//!
//! Three places could have been quadratic, and each is bounded on purpose:
//!
//! - **The writer stops at `cap`**, so a multi-megabyte line costs the cap and
//!   not the line. That is what bounds every other loop here too.
//! - **The link-target scan** stops after [`MAX_LINK_TARGET`] bytes, so each `]`
//!   costs a constant rather than a scan to end of input — `](](](…` repeated is
//!   the shape an unbounded scan is quadratic on. It is also skipped entirely
//!   when no `[` is open, which is that exact input.
//! - **Erasing a confirmed link's `[`** would be an `O(n)` `String::remove` per
//!   link. The offsets are collected instead and spliced out in one final pass.

/// Per assistant or user message, before the session cap is applied.
///
/// 8 KiB is a long message and a short file. Tunable once #439 has measured a
/// real corpus; that is why these are constants in one place rather than
/// literals at the call sites.
pub const MESSAGE_CAP: usize = 8 * 1024;

/// Per tool result. Deliberately a quarter of [`MESSAGE_CAP`]: a tool result is
/// the one input that is routinely megabytes (a file read, a long diff), it is
/// the lowest-weighted column in the ranking, and what someone remembers from
/// one is a short error string near the start.
pub const TOOL_RESULT_CAP: usize = 2 * 1024;

/// Across one whole session — every column and every message together.
///
/// The bound that actually keeps the index small: without it a session with four
/// hundred messages is four hundred times [`MESSAGE_CAP`].
pub const SESSION_CAP: usize = 512 * 1024;

/// How far past a link's `(` a matching `)` is looked for.
///
/// A constant rather than "to the end of the input" is what keeps
/// [`normalize_text`] linear. It is also generous: a URL longer than 2 KiB is
/// not one a reader would recognise, and giving up means "not a link", so the
/// text is kept — the safe direction, since a rule may only drop syntax.
pub const MAX_LINK_TARGET: usize = 2 * 1024;

/// Normalize one string, capped at `cap` bytes.
///
/// The output is never longer than `cap` and always ends on a `char` boundary —
/// truncation happens *before* a character is written, never by slicing after
/// the fact, so a multi-byte character is either fully present or fully absent
/// and there is no index to get wrong.
///
/// `cap == 0` yields an empty string, which is what makes an exhausted session
/// budget a no-op rather than a special case.
pub fn normalize_text(input: &str, cap: usize) -> String {
    if cap == 0 || input.is_empty() {
        return String::new();
    }

    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len().min(cap) + 1);
    // Byte offsets into `out` of `[` characters not yet matched by a `](`.
    let mut opens: Vec<usize> = Vec::new();
    // Offsets of the markers a confirmed link left behind, spliced out at the
    // end so no single removal has to shift the tail.
    let mut erase: Vec<usize> = Vec::new();
    // One pending space, emitted only when something follows it — which trims
    // the leading and trailing runs without needing a second pass.
    let mut pending_space = false;
    let mut in_fence = false;

    let mut i = 0usize;
    while i < bytes.len() {
        let c = input[i..].chars().next().expect("i is a char boundary");
        let width = c.len_utf8();

        // Whitespace and control characters alike collapse to one space; a
        // control character is never a word, and leaving one in would put it in
        // a snippet and in the stored column.
        if c.is_whitespace() || c.is_control() {
            pending_space = !out.is_empty();
            i += width;
            continue;
        }

        match c {
            '<' if looks_like_tag(&input[i + width..]) => {
                // Skip to the closing `>`. With none, the rest of the input is
                // inside a tag — which is what a browser would decide too.
                match input[i..].find('>') {
                    Some(rel) => i += rel + 1,
                    None => i = bytes.len(),
                }
                continue;
            }
            '`' => {
                let run = run_length(&input[i..], b'`');
                i += run;
                if run >= 3 {
                    if in_fence {
                        // A closing fence: drop the backticks and nothing else.
                        // Skipping to end of line here would eat the first words
                        // after a code block.
                        in_fence = false;
                    } else {
                        // An opening fence: the rest of the line is the info
                        // string (`rust`, `json`), which is syntax, not content.
                        in_fence = true;
                        i += input[i..].find('\n').unwrap_or(bytes.len() - i);
                    }
                }
                // A run of one or two is inline code: the delimiters go, the
                // code stays.
                continue;
            }
            ']' if !opens.is_empty() => {
                // **Pop whether or not this turns out to be a link.** A `]` with
                // no target closes its `[` as ordinary text, and leaving that
                // opener on the stack lets it shadow the real one: in
                // `[see [RFC 7231] section 4](url)` the final `]` would then pop
                // the *inner* bracket and erase the wrong character, keeping the
                // outer `[` and dropping one the reader typed.
                let open = opens.pop().expect("checked non-empty");
                if let Some(end) = link_target_end(input, i + width) {
                    erase.push(open);
                    // An image's `!` is part of the same syntax. It can only be
                    // directly adjacent, because `! [x](y)` is not image syntax.
                    if open > 0 && out.as_bytes()[open - 1] == b'!' {
                        erase.push(open - 1);
                    }
                    // Both the `]` and the whole `(target)` are dropped.
                    i = end;
                    continue;
                }
                // Not a link: `]` is ordinary punctuation and falls through, and
                // the `[` it closed stays in the output as the reader wrote it.
            }
            _ => {}
        }

        if pending_space {
            if out.len() + 1 + width > cap {
                break;
            }
            out.push(' ');
            pending_space = false;
        }
        if out.len() + width > cap {
            break;
        }
        // Recorded here rather than in the match arm above, so the offset is
        // where the `[` actually lands — after any pending space was flushed.
        if c == '[' {
            opens.push(out.len());
        }
        out.push(c);
        i += width;
    }

    splice_out(out, erase)
}

/// Remove the recorded single-byte markers in one pass.
///
/// The offsets arrive in stack order rather than sorted, so they are sorted
/// here; every one addresses an ASCII `[` or `!`, so each slice boundary is a
/// `char` boundary by construction.
fn splice_out(out: String, mut erase: Vec<usize>) -> String {
    if erase.is_empty() {
        return out;
    }
    erase.sort_unstable();
    erase.dedup();
    let mut result = String::with_capacity(out.len() - erase.len());
    let mut prev = 0usize;
    for pos in erase {
        result.push_str(&out[prev..pos]);
        prev = pos + 1;
    }
    result.push_str(&out[prev..]);
    result
}

/// `true` when `rest` — the text directly after a `<` — begins like a tag.
///
/// Requiring this is what keeps `a < b` and `x <- y` intact: a `<` followed by a
/// space, a digit or an operator is arithmetic somebody wrote, and swallowing to
/// the next `>` would delete a sentence rather than a tag.
fn looks_like_tag(rest: &str) -> bool {
    matches!(
        rest.chars().next(),
        Some(c) if c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?'
    )
}

/// How many bytes at the start of `s` are a run of the ASCII byte `b`.
fn run_length(s: &str, b: u8) -> usize {
    s.bytes().take_while(|x| *x == b).count()
}

/// If a link target starts at `at`, the byte offset just past its `)`.
///
/// Parentheses are matched by depth, because a balanced pair inside a URL is
/// legal and common (Wikipedia). Two things end the search without a match, and
/// both mean "not a link, keep the text": a newline, which ends a link target in
/// every markdown dialect and stops an unclosed `(` eating a paragraph, and
/// [`MAX_LINK_TARGET`] bytes, which is what bounds the cost per `]`.
fn link_target_end(input: &str, at: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    if bytes.get(at) != Some(&b'(') {
        return None;
    }
    let limit = (at + MAX_LINK_TARGET).min(bytes.len());
    let mut depth = 0usize;
    for (offset, b) in bytes[at..limit].iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at + offset + 1);
                }
            }
            b'\n' => return None,
            _ => {}
        }
    }
    None
}

/// One session's three text columns, accumulated under the session budget.
///
/// The shape #435 consumes: push each decoded message as it is read, then take
/// the three strings. Deliberately **not** a [`super::SearchDoc`] — the key
/// columns and the title are the indexer's to supply, and inventing a title rule
/// here would put it in the wrong file.
///
/// The budget is shared across all three columns rather than being per column,
/// because the thing being bounded is the row: a session that is all tool output
/// must not cost more than one that is all conversation.
#[derive(Debug, Clone)]
pub struct DocBuilder {
    user: String,
    assistant: String,
    tool: String,
    remaining: usize,
}

impl Default for DocBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DocBuilder {
    /// A builder with the whole [`SESSION_CAP`] still to spend.
    pub fn new() -> Self {
        Self {
            user: String::new(),
            assistant: String::new(),
            tool: String::new(),
            remaining: SESSION_CAP,
        }
    }

    /// Bytes of the session budget not yet spent.
    ///
    /// Exposed so the indexer can stop reading a transcript it can no longer
    /// index anything from, rather than normalizing megabytes into a no-op.
    pub fn remaining(&self) -> usize {
        self.remaining
    }

    pub fn push_user(&mut self, text: &str) {
        Self::push(&mut self.user, &mut self.remaining, text, MESSAGE_CAP);
    }

    pub fn push_assistant(&mut self, text: &str) {
        Self::push(&mut self.assistant, &mut self.remaining, text, MESSAGE_CAP);
    }

    pub fn push_tool(&mut self, text: &str) {
        Self::push(&mut self.tool, &mut self.remaining, text, TOOL_RESULT_CAP);
    }

    /// `(user_text, assistant_text, tool_text)`.
    pub fn into_parts(self) -> (String, String, String) {
        (self.user, self.assistant, self.tool)
    }

    fn push(field: &mut String, remaining: &mut usize, text: &str, cap: usize) {
        if *remaining == 0 {
            return;
        }
        // The joining space is spent from the budget too. Without that a session
        // of many small messages overruns by one byte per message — an overrun
        // that only appears on the largest corpus, which is the one that matters.
        let separator = usize::from(!field.is_empty());
        if *remaining <= separator {
            *remaining = 0;
            return;
        }
        let budget = (*remaining - separator).min(cap);
        let normalized = normalize_text(text, budget);
        if normalized.is_empty() {
            return;
        }
        if separator == 1 {
            field.push(' ');
        }
        field.push_str(&normalized);
        *remaining -= normalized.len() + separator;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        normalize_text(s, MESSAGE_CAP)
    }

    // ---- links ----------------------------------------------------------

    /// The acceptance criterion: the words survive, the destination does not.
    #[test]
    fn a_link_keeps_its_text_and_drops_its_target() {
        let out = norm("see [the auth guide](https://example.com/docs/v2/auth) first");
        assert_eq!(out, "see the auth guide first");
        for noise in ["example", "docs", "v2", "https"] {
            assert!(!out.contains(noise), "{noise} leaked from the target");
        }
    }

    /// An image is the same rule — the alt text is words somebody might search
    /// for; the `!` and the target are not.
    #[test]
    fn an_image_keeps_its_alt_text() {
        assert_eq!(
            norm("![the failing graph](https://example.com/g.png) shows it"),
            "the failing graph shows it"
        );
    }

    /// Brackets inside link text: the **outer** `[` is the link's, and it is the
    /// one that must be erased.
    ///
    /// This is the case that fails if a `]` without a target leaves its opener
    /// on the stack — the final `]` then pops the inner bracket, erases a `[`
    /// the reader typed, and keeps the one that is syntax.
    #[test]
    fn brackets_inside_link_text_are_handled() {
        assert_eq!(
            norm("[see [RFC 7231] section 4](https://example.com/rfc)"),
            "see [RFC 7231] section 4"
        );
        // Two levels deep, and an inner pair that *is* a link of its own.
        assert_eq!(
            norm("[outer [inner](http://i) tail](http://o)"),
            "outer inner tail"
        );
    }

    /// Several links in one string, which is what exercises the splice.
    #[test]
    fn many_links_in_one_string() {
        assert_eq!(
            norm("[one](http://a) then [two](http://b) then [three](http://c)"),
            "one then two then three"
        );
    }

    /// A **bare** URL is text the user typed and is kept — the rule is about
    /// link *syntax*, not about URLs.
    #[test]
    fn a_bare_url_is_left_alone() {
        assert_eq!(
            norm("it 404s at https://example.com/health"),
            "it 404s at https://example.com/health"
        );
    }

    /// Anything that is not a complete `[text](target)` is ordinary text, and
    /// must come out with every word intact.
    #[test]
    fn incomplete_link_syntax_keeps_every_word() {
        for (input, want) in [
            ("array[index] lookup", "array[index] lookup"),
            ("a [reference][ref] link", "a [reference][ref] link"),
            ("[unclosed target](oops", "[unclosed target](oops"),
            ("[text] (spaced)", "[text] (spaced)"),
            ("a ] stray close", "a ] stray close"),
            ("](no opener) here", "](no opener) here"),
        ] {
            assert_eq!(norm(input), want, "input: {input}");
        }
    }

    /// A newline ends a link target. Without this an unclosed `(` swallows the
    /// following paragraph — words, not syntax.
    #[test]
    fn an_unclosed_target_does_not_eat_the_next_paragraph() {
        let out = norm("[click](https://example.com\n\nthe deploy failed");
        assert!(out.contains("the deploy failed"), "got: {out}");
    }

    /// Balanced parentheses inside a target are part of the target.
    #[test]
    fn a_target_may_contain_balanced_parentheses() {
        assert_eq!(
            norm("[Rust (language)](https://en.wikipedia.org/wiki/Rust_(programming)) rules"),
            "Rust (language) rules"
        );
    }

    /// Past `MAX_LINK_TARGET` it is not treated as a link at all — the bound
    /// that keeps this linear must fail *safe*, keeping the text.
    #[test]
    fn an_absurdly_long_target_is_treated_as_text() {
        let long = "x".repeat(MAX_LINK_TARGET + 10);
        let out = norm(&format!("[label](https://e.com/{long}) tail"));
        assert!(out.contains("label"), "the link text must survive");
        assert!(out.contains("tail"), "the text after it must survive");
    }

    // ---- fences, inline code, tags --------------------------------------

    /// Code content is kept verbatim; only the fence and its info string go.
    /// People search for the exact text of an error.
    #[test]
    fn a_fence_loses_its_syntax_and_keeps_its_code() {
        let out = norm("before\n```rust\nlet x = compute();\n```\nafter");
        assert_eq!(out, "before let x = compute(); after");
        assert!(!out.contains("rust"), "the info string is syntax: {out}");
    }

    /// A closing fence must not have its line skipped, or the first words after
    /// a code block disappear.
    #[test]
    fn text_after_a_closing_fence_survives() {
        let out = norm("```\ncode\n``` and then the words");
        assert!(out.contains("code"), "got: {out}");
        assert!(out.contains("and then the words"), "got: {out}");
    }

    /// An error string inside a fence is the thing people actually search for.
    #[test]
    fn an_error_string_inside_a_fence_is_kept_verbatim() {
        let out = norm("```\nthread 'main' panicked at src/lib.rs:42:9\n```");
        assert_eq!(out, "thread 'main' panicked at src/lib.rs:42:9");
    }

    #[test]
    fn inline_code_keeps_its_contents() {
        assert_eq!(
            norm("run `cargo test --lib` now"),
            "run cargo test --lib now"
        );
    }

    /// An unclosed fence must not swallow the rest of the session.
    #[test]
    fn an_unclosed_fence_still_emits_its_content() {
        let out = norm("```rust\nthe code that follows\nand more");
        assert!(out.contains("the code that follows"), "got: {out}");
        assert!(out.contains("and more"), "got: {out}");
    }

    #[test]
    fn html_tags_are_stripped_and_their_text_kept() {
        assert_eq!(
            norm("<div class=\"x\">the <b>real</b> words</div>"),
            "the real words"
        );
        assert_eq!(norm("a <!-- note --> b"), "a b");
    }

    /// `<` is not always a tag, and swallowing to the next `>` would delete a
    /// sentence rather than syntax.
    #[test]
    fn a_less_than_sign_in_prose_is_not_a_tag() {
        for (input, want) in [
            ("assert a < b and c > d", "assert a < b and c > d"),
            ("x <- y is assignment", "x <- y is assignment"),
            ("count < 10 always", "count < 10 always"),
        ] {
            assert_eq!(norm(input), want, "input: {input}");
        }
    }

    // ---- whitespace ------------------------------------------------------

    #[test]
    fn whitespace_runs_collapse_and_the_edges_are_trimmed() {
        assert_eq!(norm("  a\n\n\tb   \r\n  c  "), "a b c");
        assert_eq!(norm("   "), "");
        assert_eq!(norm(""), "");
    }

    #[test]
    fn control_characters_become_spaces() {
        assert_eq!(norm("a\u{0}b\u{7}c\u{1b}d"), "a b c d");
    }

    // ---- caps ------------------------------------------------------------

    /// cap−1 / cap / cap+1 on plain ASCII.
    #[test]
    fn the_cap_is_an_inclusive_upper_bound() {
        for (len, cap) in [(9usize, 10usize), (10, 10), (11, 10)] {
            let out = normalize_text(&"a".repeat(len), cap);
            assert_eq!(out.len(), len.min(cap), "len {len} cap {cap}");
        }
    }

    /// The one that panics if truncation is done by slicing: a cap landing
    /// inside a multi-byte character. Every character here is 2 bytes, so an odd
    /// cap can never be reached exactly.
    #[test]
    fn truncation_never_splits_a_character() {
        for cap in 0..24usize {
            let out = normalize_text(&"é".repeat(12), cap);
            assert!(out.len() <= cap, "cap {cap} exceeded: {}", out.len());
            assert_eq!(out.chars().count(), cap / 2, "cap {cap}");
        }
    }

    /// Four-byte characters too, against caps that are not multiples of the
    /// character width in either direction.
    #[test]
    fn truncation_never_splits_an_emoji() {
        for cap in 0..20usize {
            let out = normalize_text(&"🙂".repeat(5), cap);
            assert!(out.len() <= cap);
            assert_eq!(out.chars().count(), cap / 4, "cap {cap}");
        }
    }

    /// A cap reached mid-word must not leave a dangling separator either.
    #[test]
    fn the_cap_accounts_for_the_collapsed_space() {
        assert_eq!(normalize_text("ab cd", 4), "ab c");
        assert_eq!(normalize_text("ab cd", 3), "ab");
        assert_eq!(normalize_text("ab cd", 2), "ab");
    }

    #[test]
    fn a_zero_cap_yields_nothing() {
        assert_eq!(normalize_text("anything at all", 0), "");
    }

    // ---- the builder -----------------------------------------------------

    /// The joining space counts against the cap. Without that a stream of small
    /// pushes overruns by one byte each.
    #[test]
    fn the_separator_is_paid_for_out_of_the_budget() {
        let mut b = DocBuilder::new();
        let before = b.remaining();
        b.push_user("alpha");
        assert_eq!(b.remaining(), before - 5);
        b.push_user("beta");
        assert_eq!(
            b.remaining(),
            before - 5 - 1 - 4,
            "the space must be charged"
        );
        let (user, _, _) = b.into_parts();
        assert_eq!(user, "alpha beta");
    }

    #[test]
    fn each_column_has_its_own_per_message_cap() {
        let mut b = DocBuilder::new();
        b.push_assistant(&"a".repeat(MESSAGE_CAP * 2));
        b.push_tool(&"t".repeat(TOOL_RESULT_CAP * 2));
        let (_, assistant, tool) = b.into_parts();
        assert_eq!(assistant.len(), MESSAGE_CAP);
        assert_eq!(tool.len(), TOOL_RESULT_CAP);
    }

    /// The session cap bounds the row, not each column — so it is reached by
    /// pushing across all three, and once reached nothing more is stored.
    #[test]
    fn the_session_cap_bounds_all_three_columns_together() {
        let mut b = DocBuilder::new();
        for _ in 0..200 {
            b.push_user(&"u".repeat(MESSAGE_CAP));
            b.push_assistant(&"a".repeat(MESSAGE_CAP));
            b.push_tool(&"t".repeat(TOOL_RESULT_CAP));
        }
        assert_eq!(b.remaining(), 0, "the budget must be fully spent");
        let (user, assistant, tool) = b.into_parts();
        let total = user.len() + assistant.len() + tool.len();
        assert!(total <= SESSION_CAP, "session cap exceeded: {total}");
        // And it is genuinely filled, rather than the loop having stopped early.
        assert!(total > SESSION_CAP - MESSAGE_CAP, "barely filled: {total}");
    }

    /// A push against an exhausted budget is a no-op, not a panic — the state
    /// every long session ends in.
    #[test]
    fn a_push_after_the_budget_is_gone_is_a_no_op() {
        let mut b = DocBuilder::new();
        for _ in 0..200 {
            b.push_user(&"a".repeat(MESSAGE_CAP));
        }
        assert_eq!(b.remaining(), 0);
        b.push_tool("late");
        let (_, _, tool) = b.into_parts();
        assert!(tool.is_empty());
    }

    /// A message that normalizes to nothing costs nothing — otherwise a session
    /// of blank messages exhausts the budget without indexing a word.
    #[test]
    fn an_empty_message_spends_no_budget() {
        let mut b = DocBuilder::new();
        let before = b.remaining();
        b.push_user("   \n\t  ");
        b.push_user("");
        assert_eq!(b.remaining(), before);
        let (user, _, _) = b.into_parts();
        assert!(user.is_empty());
    }

    // ---- determinism and cost -------------------------------------------

    #[test]
    fn it_is_deterministic() {
        let input = "a [x](http://e.com) `y` <b>z</b>\n\n```rust\ncode\n```";
        let first = norm(input);
        for _ in 0..8 {
            assert_eq!(norm(input), first);
        }
    }

    // ---- against the real tokenizer -------------------------------------

    /// **The claim this module is built on, checked rather than assumed**: FTS5's
    /// `unicode61` tokenizer treats punctuation as a separator, so markdown
    /// emphasis never forms part of a token and a phrase query matches straight
    /// across it. If that were false, this module would have to strip markers —
    /// and every test above would still pass, because they only assert on
    /// strings.
    ///
    /// Asserted through a real `session_search` table, because the tokenizer is
    /// the thing under test.
    #[test]
    fn markers_are_not_tokens_so_a_phrase_matches_across_them() {
        use crate::native::{migrate, search};

        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        migrate::apply(&mut conn).expect("apply");

        let source = "here is some **bold** text, and `some code` too";
        search::replace(
            &conn,
            &search::SearchDoc {
                session_id: "s".into(),
                project_path: "/p".into(),
                assistant_text: norm(source),
                ..Default::default()
            },
        )
        .expect("index");

        for phrase in [
            "\"some bold text\"",
            "\"bold text\"",
            "\"some code\"",
            "bold",
        ] {
            assert_eq!(
                search::search(&conn, phrase, 10).expect("search").len(),
                1,
                "{phrase} did not match"
            );
        }
    }

    /// The other half of the same claim: the normalizer must not have *created*
    /// a match either. A link's target is gone from the index, not merely from
    /// the string.
    #[test]
    fn a_link_target_is_not_searchable_after_normalization() {
        use crate::native::{migrate, search};

        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        migrate::apply(&mut conn).expect("apply");

        search::replace(
            &conn,
            &search::SearchDoc {
                session_id: "s".into(),
                project_path: "/p".into(),
                assistant_text: norm(
                    "read [the auth guide](https://zircondrift.example/v2/quasarflux)",
                ),
                ..Default::default()
            },
        )
        .expect("index");

        assert_eq!(search::search(&conn, "auth", 10).expect("q").len(), 1);
        assert!(search::search(&conn, "zircondrift", 10)
            .expect("q")
            .is_empty());
        assert!(search::search(&conn, "quasarflux", 10)
            .expect("q")
            .is_empty());
    }

    /// The pathological shapes named in the issue. The assertion is a *bound*
    /// rather than a benchmark — CI machines vary — but a quadratic
    /// implementation misses it by orders of magnitude, not by a margin.
    #[test]
    fn pathological_input_does_not_blow_up() {
        let cases = [
            // One multi-megabyte line.
            "x".repeat(4 * 1024 * 1024),
            // Deeply nested markdown: 200k unmatched openers.
            "[".repeat(200_000),
            // The shape an unbounded target scan is quadratic on.
            "](".repeat(200_000),
            // ...and one where every scan nearly succeeds before giving up.
            "[a](b".repeat(200_000),
            // Dense real links, which exercise the erase-and-splice path.
            "[t](https://e.com/x) ".repeat(50_000),
            // A tag that never closes, over a long input.
            format!("<div {}", "a".repeat(1024 * 1024)),
        ];
        let started = std::time::Instant::now();
        for case in &cases {
            let out = normalize_text(case, MESSAGE_CAP);
            assert!(out.len() <= MESSAGE_CAP);
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "took {elapsed:?}, which is the shape of a quadratic scan"
        );
    }
}
