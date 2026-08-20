//! Proving a ported route answers what Go answers.
//!
//! The bar for a port is a byte-identical response, and the only way to know is
//! to ask both. [`compare`] describes the first difference with the offset and
//! the surrounding bytes, which is what turns "the numbers look slightly off"
//! into "byte 4,181, `0.30000000000000004` vs `0.3`".
//!
//! **It has no caller left.** Both were removed with the implementation they
//! compared against: the proxy's shadow-diff mode (`AGENTO_DESKTOP_NATIVE=diff`)
//! died with the sidecar in #278, and the live parity harness
//! (`tests/parity_common/`, driven by `scripts/parity-instance.sh` against a Go
//! server built from the checkout) died with the Go tree in #388. It is kept
//! because it is the tool a *future* byte-comparison would want and it is
//! self-contained and tested; if nothing has used it in a year, delete it.

/// The outcome of comparing two responses for the same request.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Identical,
    Differs(String),
}

/// Compare two response bodies and describe the first difference.
///
/// Byte offsets rather than a structural JSON diff, deliberately: key order,
/// float spelling and escaping are exactly the divergences being hunted, and a
/// structural diff would call all three of them equal.
pub fn compare(go: &[u8], native: &[u8]) -> Outcome {
    if go == native {
        return Outcome::Identical;
    }

    let at = go
        .iter()
        .zip(native.iter())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| go.len().min(native.len()));

    Outcome::Differs(format!(
        "byte {at} of {} (go) / {} (native)\n  go:     …{}…\n  native: …{}…",
        go.len(),
        native.len(),
        window(go, at),
        window(native, at),
    ))
}

/// A readable slice of a body around the offset that differs.
fn window(body: &[u8], at: usize) -> String {
    const CONTEXT: usize = 60;
    let start = at.saturating_sub(CONTEXT);
    let end = (at + CONTEXT).min(body.len());
    String::from_utf8_lossy(&body[start..end])
        .escape_debug()
        .to_string()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_bodies_are_identical() {
        assert_eq!(compare(b"{\"a\":1}\n", b"{\"a\":1}\n"), Outcome::Identical);
    }

    #[test]
    fn a_float_spelling_difference_is_located_not_glossed_over() {
        let outcome = compare(b"{\"rate\":3}\n", b"{\"rate\":3.0}\n");
        let Outcome::Differs(detail) = outcome else {
            panic!("expected a mismatch");
        };
        // The bodies agree up to the digit; the difference is what follows it.
        assert!(detail.contains("byte 9"), "{detail}");
    }

    #[test]
    fn a_truncated_body_reports_the_length_difference() {
        let outcome = compare(b"{\"a\":1}\n", b"{\"a\":1}");
        let Outcome::Differs(detail) = outcome else {
            panic!("expected a mismatch");
        };
        assert!(detail.contains("8 (go) / 7 (native)"), "{detail}");
    }
}
