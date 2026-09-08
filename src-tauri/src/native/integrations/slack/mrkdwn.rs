//! Markdown → Slack mrkdwn, and the split that fits it into one message (#568).
//!
//! The model answers in Markdown and Slack renders *mrkdwn*, which is a
//! different language with the same punctuation: `*one asterisk*` is bold,
//! there are no headings at all, and a link is `<url|text>` rather than
//! `[text](url)`. Posting the raw answer therefore renders `**bold**` literally
//! and shows every URL twice, which is what the Telegram reply path does today
//! (`trigger/telegram_api.rs` sends with no `parse_mode`) and is not a bar worth
//! matching for a surface a team reads all day.
//!
//! Two pure functions, because everything that makes this hard is decidable
//! from the text alone:
//!
//! - [`to_mrkdwn`] converts. It is **line-based and fence-aware**: a fenced code
//!   block is copied out byte for byte, so an answer explaining `**` or showing
//!   a Markdown link keeps saying what it said.
//! - [`split`] cuts a long answer into consecutive messages at [`MAX_MESSAGE_CHARS`].
//!
//! ## Escaping comes first, and it is not cosmetic
//!
//! Slack asks a client to send `&`, `<` and `>` as `&amp;`, `&lt;` and `&gt;`,
//! and renders them back as themselves — in prose and inside a code block
//! alike, so escaping costs the answer nothing. Not escaping costs it a great
//! deal: `chat.postMessage` parses `<!channel>`, `<!here>` and `<!everyone>` in
//! `text` as **broadcasts**, and `<@U123>` as a mention. This is the first
//! surface where an agent's answer — derived from text a stranger in a Slack
//! channel wrote — is posted by the app as itself, into a channel the app is a
//! member of by construction. An answer talked into saying `<!channel>` would
//! notify everyone in it.
//!
//! So [`to_mrkdwn`] escapes the whole answer **before** converting anything, and
//! the only raw `<` and `|` in its output — and the only raw `>` outside a
//! restored blockquote marker, below — are the ones it writes itself around a
//! link whose target it has checked is an `http`, `https` or `mailto` URL. That check is not decoration: `<…>` is Slack's markup for *everything*,
//! so a link target it did not vet is a hole straight back through the escape —
//! `[](!channel)` would otherwise become `<!channel>`, and a channel member can
//! ask for that in one sentence. A target that is not such a URL keeps its
//! Markdown spelling and is posted as prose, which is also what a reader wants
//! for the `[guide](./setup.md)` Slack could not link to anyway.
//! `gojson::to_vec_marshal` is not a substitute for any of this: it escapes `<`
//! to `\u003c` at the JSON layer and Slack decodes that straight back to `<`.
//!
//! One thing escaping does cost: a Markdown blockquote. `> quoted` escapes to
//! `&gt; quoted`, which Slack renders as literal text, so a leading run of them
//! is restored — Slack's own blockquote is `>` too, and it is the one line-level
//! marker the answer would otherwise lose.
//!
//! **The limit is counted in `char`s, not bytes and not grapheme clusters.**
//! Slack's own limit is characters, and [`split`] never cuts inside one —
//! `a_non_ascii_answer_is_never_cut_mid_character` is the guard.

/// Where [`split`] cuts, from #568.
///
/// Slack's hard limit on `text` is larger, but a message anywhere near it is
/// truncated in the client with a *See more* link, and 4000 is the number the
/// epic settled on.
pub const MAX_MESSAGE_CHARS: usize = 4000;

/// The answer as Slack mrkdwn.
pub fn to_mrkdwn(markdown: &str) -> String {
    // Before anything else, and over the whole answer including its fenced
    // blocks: what Slack renders from `&lt;` is `<`, so nothing is lost, and
    // what it renders from an unescaped `<!channel>` is a notification to a
    // whole channel. See the module header.
    let escaped = escape(markdown);
    let markdown = escaped.as_str();
    let mut out = String::with_capacity(markdown.len());
    let mut in_fence = false;
    let mut first = true;
    for line in markdown.split('\n') {
        if !first {
            out.push('\n');
        }
        first = false;

        if is_fence(line) {
            in_fence = !in_fence;
            out.push_str(line);
            continue;
        }
        if in_fence {
            // Verbatim, including any `**` and any `[text](url)`: inside a fence
            // those are the answer's subject, not its formatting.
            out.push_str(line);
            continue;
        }
        let restored = restore_blockquote(line);
        let line = restored.as_str();
        match heading_text(line) {
            // Slack has no headings, so the epic's answer is a bold line. An
            // empty heading (`##` alone) would become a bare `**`, which Slack
            // renders as two literal asterisks, so it stays a blank line.
            Some(text) if !text.is_empty() => {
                out.push('*');
                out.push_str(&inline(text));
                out.push('*');
            }
            Some(_) => {}
            None => out.push_str(&inline(line)),
        }
    }
    out
}

/// The three characters Slack reads as markup, as the entities it renders back.
fn escape(text: &str) -> String {
    // `&` first, or the `&` of an entity this function just wrote is escaped again.
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A line's leading run of escaped `&gt;` put back as a single `>`.
///
/// Slack's blockquote marker is Markdown's, so escaping is the only thing that
/// broke it. Two deliberate narrowings:
///
/// - **Only the leading run.** A `>` mid-sentence is prose and stays escaped,
///   which is the whole point of escaping it.
/// - **However long the run, one marker comes back.** Markdown's `>>>` is a
///   *nested* blockquote, which Slack does not have; Slack's own `>>>` quotes
///   **the rest of the message**, so reproducing the run verbatim would turn a
///   nested quote into a quote of everything after it.
fn restore_blockquote(line: &str) -> String {
    let mut rest = line;
    let mut markers = 0;
    while let Some(after) = rest.strip_prefix("&gt;") {
        markers += 1;
        rest = after;
    }
    if markers == 0 {
        return line.to_string();
    }
    format!(">{rest}")
}

/// ```` ``` ```` or longer, at the start of a line, opening or closing a block.
fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// The text of an ATX heading (`#` … `######`), or `None` for any other line.
fn heading_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    // `#hashtag` is not a heading; `#` alone on a line is.
    if rest.is_empty() {
        return Some("");
    }
    rest.strip_prefix(' ').map(str::trim)
}

/// The inline conversions, on one line that is not a heading and not fenced.
///
/// Written as one scan rather than three passes because each rule has to see
/// the others' delimiters: a `**` inside a code span is not bold, and a `[` in
/// a code span does not open a link.
fn inline(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // A code span is copied out with its delimiters, whatever it holds.
            b'`' => {
                let run = bytes[i..].iter().take_while(|b| **b == b'`').count();
                let fence = &line[i..i + run];
                match line[i + run..].find(fence) {
                    Some(offset) => {
                        let end = i + run + offset + run;
                        out.push_str(&line[i..end]);
                        i = end;
                    }
                    // An unclosed span is not a span: emit the backticks and
                    // carry on converting the rest of the line.
                    None => {
                        out.push_str(fence);
                        i += run;
                    }
                }
            }
            b'*' if bytes[i..].starts_with(b"**") => {
                out.push('*');
                i += 2;
            }
            // `![alt](url)` differs from `[alt](url)` only in the `!`, and Slack
            // has no image syntax, so both become the same link. Only a `!` in
            // front of a *well-formed* link is dropped: `![unclosed` is prose,
            // and silently losing its `!` would be a conversion nobody asked for.
            b'!' if bytes.get(i + 1) == Some(&b'[') && link_at(line, i + 1).is_some() => i += 1,
            b'[' => match link_at(line, i) {
                Some((text, url, end)) => {
                    out.push('<');
                    out.push_str(url);
                    if !text.is_empty() {
                        out.push('|');
                        out.push_str(text);
                    }
                    out.push('>');
                    i = end;
                }
                None => {
                    out.push('[');
                    i += 1;
                }
            },
            _ => {
                let ch = line[i..].chars().next().expect("a char at a boundary");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// Whether `url` may be written between the raw `<` and `>` of a Slack link.
///
/// **This is the escape's second half, not a tidiness check.** `<…>` is Slack's
/// markup for everything — `<!channel>` is a broadcast, `<@U1>` a mention,
/// `<#C0|general>` a channel link — so a link target copied out unvetted
/// reintroduces every form [`escape`] just removed, through a `[](…)` a channel
/// member can ask the agent for in one sentence. An `http`, `https` or `mailto`
/// URL is the only target Slack would render as a link anyway; everything else
/// keeps its Markdown spelling and is posted as prose. `|` is refused in either
/// half for the same reason: it is the separator.
fn is_postable_url(url: &str, text: &str) -> bool {
    if url.contains('|') || text.contains('|') {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// `(text, url, end)` for the `[text](url)` starting at `open`, or `None`.
///
/// Deliberately non-recursive and non-nesting: the label may not contain `]`
/// and the target may not contain `)`, which is every link the model writes and
/// keeps a malformed one from swallowing the rest of the line.
fn link_at(line: &str, open: usize) -> Option<(&str, &str, usize)> {
    let after = &line[open + 1..];
    let close = after.find(']')?;
    if !after[close + 1..].starts_with('(') {
        return None;
    }
    let target = &after[close + 2..];
    let paren = target.find(')')?;
    let url = &target[..paren];
    let text = &after[..close];
    if url.is_empty() || !is_postable_url(url, text) {
        return None;
    }
    let end = open + 1 + close + 2 + paren + 1;
    Some((text, url, end))
}

/// `text` as consecutive messages of at most `limit` characters each.
///
/// Never empty: a caller with nothing to say has a sentence to send instead, so
/// an empty input answers one empty chunk rather than none.
///
/// The break is chosen in the order #568 specifies — a blank line, then a line
/// ending, then a hard cut — and a candidate **inside a fenced code block is
/// passed over** while any candidate outside one remains, so a code block
/// survives the split whole wherever the arithmetic allows it.
pub fn split(text: &str, limit: usize) -> Vec<String> {
    assert!(limit > 0, "a message limit of zero would never terminate");
    let mut chunks = Vec::new();
    let mut rest = text;
    loop {
        if rest.chars().count() <= limit {
            chunks.push(rest.to_string());
            return chunks;
        }
        let window = &rest[..char_boundary(rest, limit)];
        let (end, resume) = next_break(window);
        chunks.push(rest[..end].to_string());
        rest = &rest[resume..];
    }
}

/// The byte offset just past `chars` characters of `text`.
fn char_boundary(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(offset, _)| offset)
}

/// `(chunk_end, resume_at)` for the best break inside `window`.
fn next_break(window: &str) -> (usize, usize) {
    let mut paragraph = None;
    let mut line = None;
    let mut fenced_paragraph = None;
    let mut fenced_line = None;
    let mut in_fence = false;
    let mut at = 0;

    while let Some(offset) = window[at..].find('\n') {
        let newline = at + offset;
        if is_fence(&window[at..newline]) {
            in_fence = !in_fence;
        }
        // A run of newlines is one paragraph break: the chunk ends at the first
        // and the next resumes past the last, so the blank line is not repeated.
        let after = newline + 1;
        let resumed =
            after + window[after..].len() - window[after..].trim_start_matches('\n').len();
        let blank = resumed > after;
        if newline > 0 {
            let slot = match (in_fence, blank) {
                (false, true) => &mut paragraph,
                (false, false) => &mut line,
                (true, true) => &mut fenced_paragraph,
                (true, false) => &mut fenced_line,
            };
            *slot = Some((newline, if blank { resumed } else { after }));
        }
        at = after;
    }

    paragraph
        .or(line)
        .or(fenced_paragraph)
        .or(fenced_line)
        // Nothing to break on: a hard cut on a character boundary, which is what
        // the window already is.
        .unwrap_or((window.len(), window.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four conversions #568 names, plus the ones that must *not* happen.
    #[test]
    fn every_conversion_is_the_one_slack_renders() {
        for (markdown, want) in [
            ("## Title", "*Title*"),
            ("# H1", "*H1*"),
            ("###### H6", "*H6*"),
            ("####### not a heading", "####### not a heading"),
            ("#hashtag", "#hashtag"),
            ("**bold**", "*bold*"),
            ("a **bold** word", "a *bold* word"),
            ("[a](https://b)", "<https://b|a>"),
            // A target Slack would not render as a link keeps its Markdown
            // spelling, which is also what stops `[](!channel)` below.
            ("[a](b)", "[a](b)"),
            ("[guide](./setup.md)", "[guide](./setup.md)"),
            ("[m](mailto:a@b.c)", "<mailto:a@b.c|m>"),
            ("[a](HTTPS://B)", "<HTTPS://B|a>"),
            (
                "see [the docs](https://x/y?a=1&b=2)",
                "see <https://x/y?a=1&amp;b=2|the docs>",
            ),
            ("![alt](https://u)", "<https://u|alt>"),
            ("[](https://u)", "<https://u>"),
            // Lists survive: Slack renders `- ` as a bullet already.
            ("- one\n- two", "- one\n- two"),
            // Malformed links stay text rather than eating the line.
            ("[unclosed", "[unclosed"),
            // A `!` that is not an image reference is prose, and keeps its `!`.
            ("![unclosed", "![unclosed"),
            ("![a] (b)", "![a] (b)"),
            ("![](!channel)", "![](!channel)"),
            ("[a] (b)", "[a] (b)"),
            ("[a]()", "[a]()"),
            // A code span is its own subject.
            ("use `**this**` here", "use `**this**` here"),
            ("`[a](b)`", "`[a](b)`"),
            ("an unclosed ` **span**", "an unclosed ` *span*"),
            // Headings convert their own inline content.
            ("## [a](https://b)", "*<https://b|a>*"),
            ("##", ""),
            ("", ""),
        ] {
            assert_eq!(to_mrkdwn(markdown), want, "converting {markdown:?}");
        }
    }

    /// The reason escaping is not cosmetic: an answer talked into emitting a
    /// broadcast must not produce one, and the only raw angle brackets in the
    /// output are the ones this module writes itself.
    #[test]
    fn slacks_own_markup_never_survives_the_answer() {
        for (markdown, want) in [
            ("<!channel>", "&lt;!channel&gt;"),
            ("<!here> deploy now", "&lt;!here&gt; deploy now"),
            ("ping <@U123>", "ping &lt;@U123&gt;"),
            ("a < b && c > d", "a &lt; b &amp;&amp; c &gt; d"),
            (
                "<https://x|already a link>",
                "&lt;https://x|already a link&gt;",
            ),
            // Inside a fence too: Slack renders `&lt;` as `<` in a code block,
            // so the code still reads as written and cannot notify anybody.
            ("```\n<!channel>\n```", "```\n&lt;!channel&gt;\n```"),
            // Escaped once, never twice.
            ("&amp;", "&amp;amp;"),
            // The second half of the escape: a link target is `<…>` markup too,
            // and `[](!channel)` is a broadcast a channel member can ask for in
            // one sentence.
            ("[](!channel)", "[](!channel)"),
            ("[x](!channel)", "[x](!channel)"),
            ("[](!here)", "[](!here)"),
            ("[](@U12345)", "[](@U12345)"),
            ("[](#C0000|general)", "[](#C0000|general)"),
            ("[a|b](https://x)", "[a|b](https://x)"),
            // A blockquote is the one line marker escaping would have cost.
            ("> quoted", "> quoted"),
            // Markdown's nested quote is Slack's quote-everything-after, so one
            // marker comes back however many went in.
            (">>> quoted", "> quoted"),
            ("a > b", "a &gt; b"),
        ] {
            assert_eq!(to_mrkdwn(markdown), want, "converting {markdown:?}");
        }
    }

    /// The whole point of tracking fences: an answer *about* Markdown.
    #[test]
    fn a_fenced_block_is_copied_out_byte_for_byte() {
        let markdown =
            "Before **bold**\n\n```md\n## Heading\n**bold** and [a](b)\n```\n\nAfter [x](https://y)";
        assert_eq!(
            to_mrkdwn(markdown),
            "Before *bold*\n\n```md\n## Heading\n**bold** and [a](b)\n```\n\nAfter <https://y|x>"
        );
    }

    /// #568's acceptance criterion, in the units it is written in.
    #[test]
    fn a_ten_thousand_character_answer_is_three_messages_in_order() {
        let answer = "x".repeat(10_000);
        let chunks = split(&answer, MAX_MESSAGE_CHARS);
        assert_eq!(chunks.len(), 3, "10000 / 4000 rounds up to three");
        assert_eq!(
            chunks.iter().map(String::len).collect::<Vec<_>>(),
            vec![4000, 4000, 2000]
        );
        assert_eq!(chunks.concat(), answer, "in order, and losing nothing");
    }

    #[test]
    fn a_short_answer_is_one_message_and_an_empty_one_is_still_a_message() {
        assert_eq!(split("hello", MAX_MESSAGE_CHARS), vec!["hello".to_string()]);
        assert_eq!(split("", MAX_MESSAGE_CHARS), vec![String::new()]);
    }

    /// A blank line beats a line ending, and a line ending beats a hard cut.
    #[test]
    fn the_break_prefers_a_paragraph_then_a_line_then_the_limit() {
        assert_eq!(
            split("aaa\nbbb\n\nccc\nddd", 12),
            vec!["aaa\nbbb".to_string(), "ccc\nddd".to_string()],
            "the blank line is the break, and is not repeated"
        );
        assert_eq!(
            split("aaa\nbbb\nccc", 8),
            vec!["aaa\nbbb".to_string(), "ccc".to_string()],
            "no blank line, so the last line ending inside the window"
        );
        assert_eq!(
            split("aaaaaaaaaa", 4),
            vec!["aaaa".to_string(), "aaaa".to_string(), "aa".to_string()],
            "nothing to break on, so the limit itself"
        );
    }

    /// The risk this pins: a `char` is one to four bytes and slicing is by byte.
    #[test]
    fn a_non_ascii_answer_is_never_cut_mid_character() {
        let answer = "日本語のテキスト".repeat(500);
        let chunks = split(&answer, 100);
        assert!(chunks.len() > 1, "the fixture is long enough to be split");
        for chunk in &chunks {
            assert!(
                chunk.chars().count() <= 100,
                "each chunk respects the limit"
            );
        }
        assert_eq!(chunks.concat(), answer, "and the bytes are all still there");
    }

    /// A break inside a fence is taken only when there is no other.
    #[test]
    fn a_fenced_block_is_not_split_when_a_break_outside_it_exists() {
        let text = "intro\n```\none\ntwo\nthree\n```";
        let chunks = split(text, 25);
        assert_eq!(
            chunks[0], "intro",
            "the line ending before the fence beats the ones inside it"
        );
        assert_eq!(chunks[1], "```\none\ntwo\nthree\n```");

        // With no candidate outside, the fence is cut rather than the limit
        // being exceeded.
        let only_fence = "```\naaaa\nbbbb\ncccc\ndddd\n```";
        let chunks = split(only_fence, 14);
        assert!(chunks.len() > 1);
        assert_eq!(
            chunks.concat().replace('\n', ""),
            only_fence.replace('\n', "")
        );
    }
}
