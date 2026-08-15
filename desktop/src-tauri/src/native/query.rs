//! Reading a query parameter the way Go's `r.URL.Query().Get` reads it.
//!
//! Two behaviours that a `strip_prefix` scan does not have, and both reach
//! ported handlers from the frontend:
//!
//! - **Percent-decoding.** `?path=%2Fhome%2Fu` is `/home/u` to Go. A path
//!   parameter carries separators and spaces, so this is routine rather than
//!   exotic.
//! - **First value for a repeated key.** `Get` returns `Values[key][0]`.
//!
//! It lives here rather than in one of the three modules that need it because
//! it was written three times before it was written once — each handler layers
//! its own rule on top (a clamp, a positive-only default, none at all), but the
//! decoding underneath has to be Go's in all of them, and three copies of it
//! are three chances for the next port to differ from its neighbours without
//! anything failing.

/// The first decoded value for `key`, or `""` when the key is absent — which is
/// also what `Get` answers, so a caller cannot tell "absent" from "empty" and
/// neither can Go's.
pub fn value(query: &str, key: &str) -> String {
    form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_key_is_the_empty_string() {
        assert_eq!(value("", "limit"), "");
        assert_eq!(value("other=1", "limit"), "");
        // Present but empty is the same answer, exactly as `Get` gives.
        assert_eq!(value("limit=", "limit"), "");
    }

    #[test]
    fn values_are_percent_decoded() {
        assert_eq!(value("path=%2Fhome%2Fu%20x", "path"), "/home/u x");
        // …and so is the key, which is why a prefix scan is not equivalent.
        assert_eq!(value("%70ath=/a", "path"), "/a");
    }

    #[test]
    fn a_repeated_key_answers_with_its_first_value() {
        assert_eq!(value("limit=7&limit=9", "limit"), "7");
    }

    /// A key that merely ends with the one asked for must not match.
    #[test]
    fn a_longer_key_is_not_a_match() {
        assert_eq!(value("xlimit=7", "limit"), "");
        assert_eq!(value("limits=7", "limit"), "");
    }
}
