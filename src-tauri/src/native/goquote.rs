//! Go's `strconv.Quote`, which is what `fmt.Sprintf("%q", s)` does to a string.
//!
//! # Why a JSON encoder is not this
//!
//! The five token validators behind `POST /api/integrations/{id}/auth/validate`
//! build the `auth` column with a format string rather than a marshaller:
//!
//! ```go
//! cfg.Auth = json.RawMessage(fmt.Sprintf(`{"validated":true,"team_name":%q}`, teamName))
//! ```
//!
//! `%q` is a **Go string literal**, not a JSON string, and the two disagree in
//! three ways that all reach the stored column:
//!
//! - `encoding/json` HTML-escapes `<`, `>` and `&`; `Quote` leaves them. A Slack
//!   workspace called `A & B` is stored as `"A & B"`, never `"A & B"`.
//! - `Quote` writes a control character as `\x01`, which is **not valid JSON** —
//!   Go stores it anyway, and a reader that decodes the column then fails. That
//!   is Go's behavior, so it is this port's.
//! - the key order is the format string's, not `encoding/json`'s sorted map
//!   order: `validated` comes first.
//!
//! So the payload is built by string concatenation here too. Reaching for
//! `serde_json` would be the "`Value` as a way-station" parity smell
//! `desktop/CLAUDE.md` warns about, one level up: it would produce *valid* JSON
//! that differs from Go's bytes in every one of the three ways above.
//!
//! # Why the printable table travels rather than being approximated
//!
//! Every non-ASCII rune's fate is decided by `strconv.IsPrint`, and Rust has no
//! equivalent. `char::escape_debug` looks like one and is not: it disagrees with
//! `strconv.IsPrint` on **12,589 code points**. It escapes combining marks,
//! which Go prints — so `"cafe\u{301}"`, an entirely ordinary display name,
//! would have been stored differently by the two implementations — and it prints
//! ~8,500 code points Go escapes.
//!
//! `desktop/parity/goquote_vectors.json` therefore carries the 711 inclusive
//! ranges `strconv.IsPrint` reports true for, generated from Go and asserted by
//! both languages, exactly as `migrations_vectors.json` carries the schema.

use std::sync::OnceLock;

/// The vectors, embedded rather than read at runtime — the shell has no
/// working directory it can rely on.
const VECTORS: &str = include_str!("../../../parity/goquote_vectors.json");

#[derive(serde::Deserialize)]
struct Vectors {
    is_print_ranges: Vec<(u32, u32)>,
}

fn print_ranges() -> &'static [(u32, u32)] {
    static RANGES: OnceLock<Vec<(u32, u32)>> = OnceLock::new();
    RANGES.get_or_init(|| {
        let parsed: Vectors =
            serde_json::from_str(VECTORS).expect("goquote_vectors.json is embedded and must parse");
        parsed.is_print_ranges
    })
}

/// `strconv.IsPrint`: letters, marks, numbers, punctuation, symbols, and the
/// ASCII space. Everything else — the C and Z categories — is escaped.
///
/// Binary search over ascending, non-overlapping ranges. The ASCII fast path is
/// not an optimization so much as the common case stated plainly: `0x20..=0x7e`
/// is the table's own first range.
pub fn is_print(c: char) -> bool {
    let cp = c as u32;
    if (0x20..0x7f).contains(&cp) {
        return true;
    }
    print_ranges()
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `strconv.Quote` — the double-quoted Go literal for `s`, quotes included.
///
/// Mirrors `appendQuotedWith` + `appendEscapedRune` with `quote = '"'`,
/// `ASCIIonly = false`, `graphicOnly = false`. The branch order is Go's, and it
/// is load-bearing: `"` and `\` are escaped **before** printability is
/// consulted, and the named escapes are only reached because `IsPrint` is false
/// for all seven.
///
/// `appendQuotedWith`'s remaining branch — a byte that is not valid UTF-8,
/// written as `\x` + its two hex digits — is unreachable here, because a Rust
/// `&str` is valid UTF-8 by construction. Go reaches it only for a string built
/// from raw bytes; every value this port quotes came out of a JSON response, and
/// `encoding/json` has already replaced invalid UTF-8 with U+FFFD by then.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
            out.push(c);
            continue;
        }
        if is_print(c) {
            out.push(c);
            continue;
        }
        match c {
            '\u{07}' => out.push_str("\\a"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0b}' => out.push_str("\\v"),
            _ => {
                let cp = c as u32;
                if cp < 0x20 || cp == 0x7f {
                    out.push_str(&format!("\\x{cp:02x}"));
                } else if cp < 0x10000 {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    out.push_str(&format!("\\U{cp:08x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct AllVectors {
        is_print_ranges: Vec<(u32, u32)>,
        quote: Vec<QuoteCase>,
        auth: Vec<AuthCase>,
    }
    #[derive(serde::Deserialize)]
    struct QuoteCase {
        value: String,
        want: String,
    }
    #[derive(serde::Deserialize)]
    struct AuthCase {
        format: String,
        value: String,
        want: String,
    }

    fn vectors() -> AllVectors {
        serde_json::from_str(VECTORS).expect("vectors parse")
    }

    /// The whole point: Go's own output, byte for byte.
    #[test]
    fn quote_matches_the_vectors_go_generated() {
        for case in vectors().quote {
            assert_eq!(quote(&case.value), case.want, "quote({:?})", case.value);
        }
    }

    /// And the five `auth` payloads composed the way the validators compose
    /// them, so the format strings are pinned alongside the primitive.
    #[test]
    fn the_auth_payloads_match_the_vectors_go_generated() {
        for case in vectors().auth {
            let (prefix, suffix) = case
                .format
                .split_once("%q")
                .expect("each auth format holds exactly one %q");
            let built = format!("{prefix}{}{suffix}", quote(&case.value));
            assert_eq!(built, case.want, "{} with {:?}", case.format, case.value);
        }
    }

    /// The ranges have to be ascending and non-overlapping or the binary search
    /// silently answers wrong for the runes in the disordered part.
    #[test]
    fn the_print_ranges_are_sorted_and_disjoint() {
        let ranges = vectors().is_print_ranges;
        assert!(!ranges.is_empty());
        for pair in ranges.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(a.0 <= a.1, "range {a:?} is inverted");
            assert!(a.1 + 1 < b.0, "ranges {a:?} and {b:?} touch or overlap");
        }
        assert_eq!(ranges[0], (0x20, 0x7e), "ASCII printables lead the table");
    }

    /// The ASCII fast path must agree with the table it shortcuts, or the two
    /// disagree exactly where nobody looks.
    #[test]
    fn the_ascii_fast_path_agrees_with_the_table() {
        let ranges = print_ranges();
        let in_table = |cp: u32| ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&cp));
        for cp in 0u32..0x100 {
            let c = char::from_u32(cp).expect("latin-1 is all valid");
            assert_eq!(is_print(c), in_table(cp), "U+{cp:04X}");
        }
    }

    /// The three ways this differs from a JSON encoder, stated as assertions so
    /// a future "just use serde_json here" fails rather than silently rewriting
    /// the column.
    #[test]
    fn quote_is_not_json_string_escaping() {
        // No HTML escaping: `encoding/json` writes &, <, >.
        assert_eq!(quote("A & B"), r#""A & B""#);
        assert_eq!(quote("<b>"), r#""<b>""#);
        // \x is a Go escape and not valid JSON at all.
        assert_eq!(quote("ctrl\u{1}char"), r#""ctrl\x01char""#);
        // A combining mark is printed, which is where char::escape_debug would
        // have diverged on an ordinary display name.
        assert_eq!(quote("cafe\u{301}"), "\"cafe\u{301}\"");
        // …while a zero-width space is not, and comes back as an escape.
        assert_eq!(quote("\u{200b}"), r#""\u200b""#);
        // Astral printables stay raw; a non-printable astral rune gets \U.
        assert_eq!(quote("emoji🙂"), "\"emoji🙂\"");
        assert_eq!(quote("\u{e0001}"), r#""\U000e0001""#);
    }
}
