//! `internal/notification/template.go` in Rust: the subject prefix and the
//! branded HTML wrapper every notification is rendered into.
//!
//! # Why this is not "just a string"
//!
//! The rendered subject and body **ship to a human**. Every other ported route
//! is verified by diffing JSON against Go, and there is no JSON here — the
//! parity bar is the mail itself, so this file is asserted against a golden
//! rendered by Go (`desktop/parity/notification_template_golden.json`, written
//! by `go test ./desktop/parity/ -update-notification-template-golden`).
//!
//! # `html/template` is not "escape the five XML characters" — nor eight
//!
//! Go interpolates `{{.Body}}` in an HTML text node through `htmlEscaper`,
//! whose `htmlReplacementTable` is **seven** entries: the usual five plus `+`
//! and NUL. So `a+b` renders as `a&#43;b`, which a general-purpose escaper does
//! not produce.
//!
//! The near miss is `=`. It looks like it belongs — it sits right beside `+` in
//! the same file — but it lives in `htmlNospaceReplacementTable`, which applies
//! to **unquoted attribute values**, not to text. A text node keeps its `=`
//! verbatim, and this port escaped it until the golden said otherwise. That
//! matters in practice rather than in theory: `Handle` builds every body from
//! `fmt.Sprintf("%s: %s", k, v)` lines, so an equals sign is in most real
//! notifications.
//!
//! This is why [`html_escape`] is written out rather than pulled from a crate,
//! and why it is checked against bytes Go produced rather than against a
//! reading of Go's source — which is how the `=` got in.

/// `notification.SubjectPrefix`, prepended to every outgoing subject.
pub const SUBJECT_PREFIX: &str = "Agento Notification - ";

/// `buildSubject`.
pub fn build_subject(subject: &str) -> String {
    format!("{SUBJECT_PREFIX}{subject}")
}

/// Go's `html/template` `htmlReplacementTable`, entry for entry.
///
/// Seven entries, and the two that are not obvious are `+` (escaped, because it
/// is significant to `Content-Type` sniffing) and NUL (U+FFFD, not an entity).
/// `=` is deliberately **absent**: it is in the *nospace* table, which governs
/// unquoted attribute values rather than text.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\0' => out.push('\u{FFFD}'),
            '"' => out.push_str("&#34;"),
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#39;"),
            '+' => out.push_str("&#43;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// `emailTmpl` as `html/template` **renders** it, not as `template.go` spells
/// it — the three actions left in place as literal markers.
///
/// The difference is not cosmetic and is the trap here. `html/template`
/// **elides HTML comments**, and the Go template carries six of them
/// (`<!-- ── Header ── -->` and friends). Lifting the source text verbatim
/// produces mail that is 271 bytes longer than Go's and differs on six lines,
/// with nothing downstream to notice. So this file is the *output* of executing
/// the Go template with sentinel values, which is why the surviving blank lines
/// where the comments were look like accidents and are not: they are the
/// whitespace either side of an elided comment, and Go emits them.
///
/// Regenerate it the same way if `template.go` changes — the golden test below
/// is what catches a stale copy.
const EMAIL_TEMPLATE: &str = include_str!("email.html");

/// `buildEmailHTML`: render the wrapper around a subject and a body.
///
/// The in-body title strips the prefix so it reads cleanly inside the email —
/// `TrimPrefix`, so a subject that does not carry the prefix is left alone
/// rather than having its first 22 characters removed.
pub fn build_email_html(subject: &str, body: &str) -> String {
    let title = subject.strip_prefix(SUBJECT_PREFIX).unwrap_or(subject);
    EMAIL_TEMPLATE
        .replacen("{{.FullSubject}}", &html_escape(subject), 1)
        .replacen("{{.Title}}", &html_escape(title), 1)
        .replacen("{{.Body}}", &html_escape(body), 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Golden {
        #[allow(dead_code)]
        _comment: Vec<String>,
        cases: Vec<Case>,
    }

    #[derive(serde::Deserialize)]
    struct Case {
        subject: String,
        body: String,
        /// Whether `want_subject` came from `build_subject`. The last case
        /// deliberately did not, so the in-body title exercises the no-match
        /// arm of `TrimPrefix`.
        prefixed: bool,
        want_subject: String,
        want_html: String,
    }

    /// The renderings Go produced for the same inputs, byte for byte.
    ///
    /// This is the only check that matters here: the file is a port of a Go
    /// template, and the thing it produces is read by a person rather than
    /// parsed by the frontend, so nothing downstream would notice a divergence.
    #[test]
    fn the_rendered_mail_matches_gos_bytes() {
        let raw = include_str!("../../../../parity/notification_template_golden.json");
        let golden: Golden = serde_json::from_str(raw).expect("golden fixture");
        assert!(!golden.cases.is_empty(), "the fixture must carry cases");

        for case in &golden.cases {
            if case.prefixed {
                assert_eq!(
                    build_subject(&case.subject),
                    case.want_subject,
                    "subject for {:?}",
                    case.subject
                );
            }
            assert_eq!(
                build_email_html(&case.want_subject, &case.body),
                case.want_html,
                "html for {:?}",
                case.subject
            );
        }
    }

    /// The two entries a five-character escaper misses, and the one it is
    /// tempting to add and must not.
    #[test]
    fn the_escaper_is_gos_seven_entry_table_and_leaves_equals_alone() {
        assert_eq!(
            build_email_html("s", "a+b=c").matches("a&#43;b=c").count(),
            1
        );
        assert!(build_email_html("s", "a\0b").contains('\u{FFFD}'));
        assert!(build_email_html("s", "<i>&'\"").contains("&lt;i&gt;&amp;&#39;&#34;"));
    }

    /// `TrimPrefix`, not "drop the first 22 characters": a subject without the
    /// prefix keeps all of it.
    #[test]
    fn a_subject_without_the_prefix_is_left_whole() {
        let html = build_email_html("Bare Subject", "b");
        assert!(html.contains(">Bare Subject</p>"), "{html}");
    }
}
