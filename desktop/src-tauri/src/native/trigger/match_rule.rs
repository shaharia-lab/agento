//! Whether an inbound Telegram message fires a trigger rule, and what prompt it
//! runs with. Mirrors `matchRule` in `internal/trigger/dispatcher.go`.
//!
//! Pinned to `desktop/parity/trigger_match_vectors.json`, generated from Go's
//! own matcher. Almost every clause here is a place the two languages disagree
//! by default, and two of them disproved what this file's author believed before
//! the vectors were generated:
//!
//! - **`text[:prefixLen]` is a byte slice, and Go will cut a character in half.**
//!   A one-byte ASCII prefix against a message starting with a three-byte
//!   character takes `text[..1]` — a lone continuation byte, invalid UTF-8 — and
//!   `EqualFold` decodes it as `RuneError`, so it does not match. Slicing by
//!   *character* would compare the whole character instead (and might match);
//!   slicing by byte in Rust would **panic**. This function therefore compares
//!   bytes and never indexes a `str`.
//! - **The length guard is bytes too.** `len(text) < prefixLen` rejects before
//!   the slice, which is what keeps the slice in range.
//! - **`EqualFold` is Unicode simple folding**, not ASCII case-insensitivity and
//!   not lower-casing: sigma folds to final sigma, while `U+0130` does *not*
//!   fold to ASCII `i` even though it lower-cases to one.
//! - **`TrimSpace` is `unicode.IsSpace`**, which includes `U+00A0` and `U+0085`.
//!   Rust's `char::is_whitespace` agrees, both following `White_Space` — two
//!   standards agreeing, which is why the vectors pin it rather than assume it.
//! - **An empty prompt after the trim is not a match.** A bare `/ask` runs
//!   nothing rather than running with an empty prompt.
//! - **Keywords are checked against the *whole* message, not the stripped
//!   prompt.** So a keyword that appears only inside the prefix still satisfies
//!   the filter — the rule fires with a prompt that does not contain it.

/// One rule's filters, which is all the matcher reads.
#[derive(Debug, Default, Clone)]
pub struct RuleFilters {
    pub prefix: String,
    pub keywords: Vec<String>,
    pub chat_ids: Vec<String>,
}

/// `matchRule`: `Some(prompt)` when the rule fires.
pub fn match_rule(filters: &RuleFilters, text: &str, chat_id: &str) -> Option<String> {
    let mut prompt = text.to_string();

    if !filters.prefix.is_empty() {
        let prefix_len = filters.prefix.len();
        let bytes = text.as_bytes();
        // `len(text) < prefixLen` — bytes on both sides.
        if bytes.len() < prefix_len {
            return None;
        }
        if !equal_fold(&bytes[..prefix_len], filters.prefix.as_bytes()) {
            return None;
        }
        // The remainder is always a char boundary when the prefix matched by
        // folding, because a fold match implies the bytes decoded as whole
        // characters. `from_utf8` rather than an index, so a future change to
        // the comparison cannot turn this into a panic.
        let rest = match std::str::from_utf8(&bytes[prefix_len..]) {
            Ok(rest) => rest,
            // Unreachable today; a mid-character split cannot have folded equal.
            Err(_) => return None,
        };
        prompt = rest.trim().to_string();
        if prompt.is_empty() {
            return None;
        }
    }

    // Deliberately `text`, not `prompt` — see the module header.
    if !matches_keywords(&filters.keywords, text) {
        return None;
    }
    if !matches_chat_ids(&filters.chat_ids, chat_id) {
        return None;
    }
    Some(prompt)
}

/// `matchesKeywords`: empty is "everything", otherwise OR over case-folded
/// `Contains`.
fn matches_keywords(keywords: &[String], text: &str) -> bool {
    if keywords.is_empty() {
        return true;
    }
    // `strings.ToLower` on both sides. Full Unicode lower-casing in both
    // languages, unlike the prefix comparison, which folds.
    let lower = text.to_lowercase();
    keywords.iter().any(|kw| lower.contains(&kw.to_lowercase()))
}

/// `matchesChatIDs`: empty is "everything", otherwise an exact string compare.
fn matches_chat_ids(allowed: &[String], chat_id: &str) -> bool {
    allowed.is_empty() || allowed.iter().any(|id| id == chat_id)
}

/// `strings.EqualFold` over bytes, with Go's UTF-8 decoding.
///
/// Byte slices rather than `&str` because the caller may hand it a slice that
/// splits a character — that is the case the vectors exist for. Go decodes such
/// a byte as `RuneError` with width 1, which is reproduced here.
fn equal_fold(a: &[u8], b: &[u8]) -> bool {
    let (mut a, mut b) = (a, b);
    loop {
        let (Some((ar, asize)), Some((br, bsize))) = (decode_rune(a), decode_rune(b)) else {
            // One or both exhausted: equal only if both are.
            return a.is_empty() && b.is_empty();
        };
        a = &a[asize..];
        b = &b[bsize..];
        if ar == br {
            continue;
        }
        if !simple_fold_eq(ar, br) {
            return false;
        }
    }
}

/// `utf8.DecodeRuneInString`: the next rune and its width, with an invalid
/// sequence decoding as `U+FFFD` of width 1.
fn decode_rune(bytes: &[u8]) -> Option<(char, usize)> {
    if bytes.is_empty() {
        return None;
    }
    // Longest valid prefix is at most 4 bytes.
    let upto = bytes.len().min(4);
    for width in 1..=upto {
        if let Ok(s) = std::str::from_utf8(&bytes[..width]) {
            if let Some(c) = s.chars().next() {
                return Some((c, width));
            }
        }
    }
    Some((char::REPLACEMENT_CHARACTER, 1))
}

/// Whether two runes are equal under Unicode **simple** folding.
///
/// Go walks `unicode.SimpleFold`'s orbit; the orbits that matter in practice are
/// reproduced through `to_lowercase`/`to_uppercase`, with the ASCII fast path
/// first because it is the overwhelmingly common case and is exact.
///
/// **This is an approximation of the full orbit table**, and the vectors are
/// what bound it: sigma's three-way orbit and `U+0130`'s *absence* from ASCII
/// `i`'s orbit are both pinned, because those are the two shapes a
/// lower-casing implementation gets wrong in opposite directions.
fn simple_fold_eq(a: char, b: char) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(&b);
    }
    // **The Turkish pair are self-orbits, in both directions.**
    // `unicode.SimpleFold` maps U+0130 and U+0131 to themselves, so
    // `EqualFold("ı", "i")` and `EqualFold("İ", "i")` are both false — verified
    // against Go. The case mappings below would say otherwise: `ı`
    // upper-cases to ASCII `I`, and `İ` lower-cases to a string starting with
    // ASCII `i`. Neither is a fold.
    if matches!(a, '\u{0130}' | '\u{0131}') || matches!(b, '\u{0130}' | '\u{0131}') {
        return false;
    }
    // `U+0130` lower-cases to "i̇" (two chars) and `U+0131` upper-cases to "I",
    // but neither shares an orbit with ASCII `i`. Comparing single-char
    // case mappings keeps them apart, where a string-wise `to_lowercase`
    // comparison would fold the first into ASCII.
    let one = |c: char, f: fn(char) -> std::char::ToLowercase| {
        let mut it = f(c);
        match (it.next(), it.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    };
    let lower = |c: char| one(c, char::to_lowercase);
    if let (Some(x), Some(y)) = (lower(a), lower(b)) {
        if x == y {
            return true;
        }
    }
    // **The uppercase direction is not redundant.** Sigma's orbit is
    // `σ → ς → Σ`, so the two lowercase forms map to themselves and only agree
    // through the capital. Lower-casing alone reports them different, which is
    // what the vectors caught.
    let upper = |c: char| {
        let mut it = c.to_uppercase();
        match (it.next(), it.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    };
    match (upper(a), upper(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Vectors {
        cases: Vec<Case>,
    }

    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        #[allow(dead_code)]
        note: String,
        filter_prefix: String,
        filter_keywords: Option<Vec<String>>,
        filter_chat_ids: Option<Vec<String>>,
        text: String,
        chat_id: String,
        matched: bool,
        prompt: String,
    }

    #[test]
    fn every_case_matches_what_go_decided() {
        let raw = include_str!("../../../../parity/trigger_match_vectors.json");
        let vectors: Vectors = serde_json::from_str(raw).expect("vectors decode");
        assert!(!vectors.cases.is_empty(), "the vector file is empty");

        for case in &vectors.cases {
            let filters = RuleFilters {
                prefix: case.filter_prefix.clone(),
                keywords: case.filter_keywords.clone().unwrap_or_default(),
                chat_ids: case.filter_chat_ids.clone().unwrap_or_default(),
            };
            let got = match_rule(&filters, &case.text, &case.chat_id);
            assert_eq!(got.is_some(), case.matched, "case {:?}: matched", case.name);
            assert_eq!(
                got.unwrap_or_default(),
                case.prompt,
                "case {:?}: prompt",
                case.name
            );
        }
    }

    #[test]
    fn a_prefix_longer_than_the_message_never_indexes_out_of_range() {
        // The guard is bytes, and this is the case that would panic without it.
        let filters = RuleFilters {
            prefix: "/askmore".to_string(),
            ..Default::default()
        };
        assert!(match_rule(&filters, "/a", "1").is_none());
        assert!(match_rule(&filters, "", "1").is_none());
    }

    #[test]
    fn a_multibyte_message_with_a_shorter_ascii_prefix_does_not_panic() {
        // `text[..1]` here is a lone continuation byte. Go compares it as
        // RuneError; the point of the byte-wise comparison is that Rust neither
        // panics nor silently compares a whole character.
        let filters = RuleFilters {
            prefix: "K".to_string(),
            ..Default::default()
        };
        assert!(match_rule(&filters, "\u{212A}elvin", "1").is_none());
        assert!(match_rule(&filters, "\u{00E9}xyz", "1").is_none());
    }

    #[test]
    fn the_turkish_pair_fold_with_nothing_but_themselves() {
        // Verified against Go: `unicode.SimpleFold` maps both U+0130 and U+0131
        // to themselves, so `EqualFold("ı","i")` and `EqualFold("İ","i")` are
        // false. The case mappings say otherwise in both directions — `ı`
        // upper-cases to ASCII `I`, `İ` lower-cases to a string starting with
        // ASCII `i` — so this is the one place folding and casing disagree
        // enough to need naming.
        assert!(!simple_fold_eq('\u{0131}', 'i'));
        assert!(!simple_fold_eq('\u{0131}', 'I'));
        assert!(!simple_fold_eq('\u{0130}', 'i'));
        assert!(!simple_fold_eq('\u{0130}', 'I'));
        // …and each still equals itself, through the `ar == br` fast path.
        assert!(equal_fold("\u{0131}".as_bytes(), "\u{0131}".as_bytes()));
        assert!(equal_fold("\u{0130}".as_bytes(), "\u{0130}".as_bytes()));
        // The sigma orbit is unaffected by the guard.
        assert!(simple_fold_eq('\u{03C2}', '\u{03C3}'));
    }

    #[test]
    fn decoding_matches_gos_widths() {
        assert_eq!(decode_rune(b"a"), Some(('a', 1)));
        assert_eq!(decode_rune("é".as_bytes()), Some(('é', 2)));
        assert_eq!(decode_rune("\u{212A}".as_bytes()), Some(('\u{212A}', 3)));
        assert_eq!(decode_rune(b""), None);
        // A lone continuation byte is RuneError of width 1, as Go decodes it.
        assert_eq!(decode_rune(&[0x80]), Some((char::REPLACEMENT_CHARACTER, 1)));
        // …and so is a truncated multi-byte sequence.
        assert_eq!(
            decode_rune(&"é".as_bytes()[..1]),
            Some((char::REPLACEMENT_CHARACTER, 1))
        );
    }
}
