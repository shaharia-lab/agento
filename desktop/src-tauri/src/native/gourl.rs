//! Go's `net/url` path encoding, and the path chi actually routes on.
//!
//! # Why the seam needs this at all
//!
//! Every claim function in `native/` matches on `Request::path`, and until #294
//! that was `req.uri().path()` — the **raw** request target, byte for byte off
//! the wire. Go's router does not see that string. `net/http` parses the URI
//! into a `url.URL` before any handler runs, and `chi` then routes on
//!
//! ```text
//! RawPath != "" ? RawPath : Path
//! ```
//!
//! (`Mux.routeHTTP`), handing whatever it captured to `chi.URLParam` **without
//! decoding it further**. So the question "is Go's path segment the encoded one
//! or the decoded one?" has no single answer — it depends on `RawPath`, and
//! `url.setPath` sets `RawPath` only when the *default* encoding of the decoded
//! path differs from what arrived:
//!
//! | request target | `URL.Path` | `RawPath` | chi's segment |
//! |---|---|---|---|
//! | `/api/agents/a-b`      | `/api/agents/a-b`   | `""`                  | `a-b` |
//! | `/api/agents/a%2Db`    | `/api/agents/a-b`   | `/api/agents/a%2Db`   | `a%2Db` |
//! | `/api/agents/a%20b`    | `/api/agents/a b`   | `""`                  | `a b` |
//! | `/api/agents/caf%C3%A9`| `/api/agents/café`  | `""`                  | `café` |
//! | `/api/agents/a%2Fb`    | `/api/agents/a/b`   | `/api/agents/a%2Fb`   | `a%2Fb` |
//!
//! Read as a rule: **Go decodes exactly when the escaping is canonical.** A
//! space or a non-ASCII byte *has* to be percent-encoded to travel, so its
//! encoding round-trips and Go hands the handler the decoded text; `%2D` for a
//! character that needs no escaping does not round-trip, so Go hands over the
//! escaped text unchanged.
//!
//! Matching on the raw path therefore agreed with Go on the second and fifth
//! rows and disagreed on the third and fourth. While every claimed route was a
//! read that was invisible — a miss produced `Err` and forwarded, and Go
//! answered correctly — but #274 and #276 claimed writes, so `agents::update`
//! now *answers* 404 and `chats::patch` answers `chat not found` for a request
//! Go would have applied to a real row.
//!
//! [`route_path`] is that whole rule in one function, applied once in
//! `proxy.rs` where `native::Request` is built, so no module's claim function
//! has to know about it and none of them can drift apart.
//!
//! There is no injection risk in either spelling — every id reaches SQLite as a
//! bound parameter — so this is a correctness fix, not a security one.
//!
//! `desktop/parity/gourl_vectors.json` is generated from Go and read by both
//! languages' tests, exactly as `gopath_vectors.json` is, so these functions are
//! pinned to what Go does rather than to what this comment believes.

/// Bytes Go's `escape(s, encodePath)` leaves alone.
///
/// Transcribed from `shouldEscape` in `net/url/url.go`, `encodePath` arm:
/// alphanumerics and the unreserved marks are never escaped; of the reserved
/// set `$ & + , / : ; = ? @` a path may carry every one **except** `?`; and
/// everything else — every byte over 0x7F included — is escaped.
fn should_escape(c: u8) -> bool {
    if c.is_ascii_alphanumeric() {
        return false;
    }
    match c {
        b'-' | b'_' | b'.' | b'~' => false,
        // The RFC allows `: @ & = + $` in a path and reserves `/ ; ,` for
        // assigning meaning to individual segments; `net/url` manipulates the
        // path as a whole and so allows those three too. That leaves `?`.
        b'$' | b'&' | b'+' | b',' | b'/' | b':' | b';' | b'=' | b'@' => false,
        b'?' => true,
        _ => true,
    }
}

/// Go's `escape(s, encodePath)`.
///
/// Uppercase hex, and **no `+` for space** — that substitution belongs to
/// `encodeQueryComponent`, and using it here would make every path holding a
/// space look non-canonical and flip [`route_path`] to the wrong branch.
pub fn escape_path(s: &str) -> String {
    let bytes = s.as_bytes();
    if !bytes.iter().copied().any(should_escape) {
        return s.to_string();
    }

    const UPPERHEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &c in bytes {
        if should_escape(c) {
            out.push('%');
            out.push(UPPERHEX[(c >> 4) as usize] as char);
            out.push(UPPERHEX[(c & 15) as usize] as char);
        } else {
            out.push(c as char);
        }
    }
    out
}

/// Go's `unescape(s, encodePath)`: `%XX` becomes one byte, everything else is
/// carried through.
///
/// `None` is Go's `EscapeError`, which `url.ParseRequestURI` returns and
/// `net/http` turns into a **400 before any handler runs**. `+` is left as `+`,
/// because the plus-is-space rule is `encodeQueryComponent`'s.
///
/// Bytes rather than a `String`: a Go string is arbitrary bytes and `%FF` is a
/// perfectly routable path to Go. Deciding what to do about one that is not
/// UTF-8 is [`route_path`]'s job.
pub fn unescape_path(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// The path chi routes on, given the raw request target's path.
///
/// `RawPath != "" ? RawPath : Path`, with `RawPath` derived the way
/// `url.setPath` derives it — see the module header for the table this produces.
///
/// `None` means **forward**, for either of two reasons, and both are the seam
/// working as designed rather than a gap:
///
/// - the escaping is malformed (`/api/agents/a%2`). `url.ParseRequestURI` fails
///   on it, so Go answers 400 from inside `net/http` and never reaches a
///   handler. Forwarding is how that 400 gets produced — reproducing it here
///   would mean reproducing the server's error page too.
/// - the decoded path is not UTF-8 (`/api/agents/%FF`). Go carries it happily;
///   Rust cannot put it in a `&str`, and every claim function and bound
///   parameter downstream wants one. Rather than lossily converting — which
///   would look up a *different* slug than Go looks up — the request forwards
///   and Go answers it correctly.
pub fn route_path(raw: &str) -> Option<String> {
    let decoded = unescape_path(raw)?;
    let decoded = String::from_utf8(decoded).ok()?;
    // `url.setPath`: the escaping is canonical exactly when re-encoding the
    // decoded path reproduces what arrived, and that is when `RawPath` stays
    // empty and chi falls through to the decoded `URL.Path`.
    if escape_path(&decoded) == raw {
        Some(decoded)
    } else {
        Some(raw.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct RoutePathCase {
        raw: String,
        /// Go's `URL.Path`, or `null` when `ParseRequestURI` rejected the target.
        path: Option<String>,
        /// Go's `URL.RawPath`.
        raw_path: Option<String>,
        /// What `chi.URLParam` would capture: `RawPath` if set, else `Path`.
        route_path: Option<String>,
    }

    #[derive(Deserialize)]
    struct EscapeCase {
        value: String,
        want: String,
    }

    #[derive(Deserialize)]
    struct Vectors {
        route_path: Vec<RoutePathCase>,
        escape_path: Vec<EscapeCase>,
    }

    fn vectors() -> Vectors {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../parity/gourl_vectors.json");
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"));
        serde_json::from_str(&raw).expect("parsing gourl vectors")
    }

    /// The whole point of the file: Rust is asserted against what Go actually
    /// produced, not against what this port believes Go produces.
    #[test]
    fn route_path_matches_the_go_vectors() {
        let v = vectors();
        assert!(v.route_path.len() >= 20, "vectors look truncated");
        for case in v.route_path {
            // `route_path: null` is a target `url.ParseRequestURI` rejects, so
            // Go answers 400 from inside `net/http` and never routes it at all.
            // The non-UTF-8 case cannot appear here — a JSON string is UTF-8 by
            // construction — so it has its own test below.
            assert_eq!(
                route_path(&case.raw),
                case.route_path,
                "route_path({:?}) — Go: Path={:?} RawPath={:?}",
                case.raw,
                case.path,
                case.raw_path
            );
        }
    }

    #[test]
    fn escape_path_matches_the_go_vectors() {
        for case in vectors().escape_path {
            assert_eq!(
                escape_path(&case.value),
                case.want,
                "escape({:?}, encodePath)",
                case.value
            );
        }
    }

    /// The four rows of the module header's table, spelled out so a reader of
    /// this file can see the rule without opening the vectors.
    #[test]
    fn the_rule_is_decode_when_the_escaping_is_canonical() {
        // Non-canonical: `-` needs no escaping, so Go keeps the escaped form.
        assert_eq!(
            route_path("/api/agents/a%2Db").as_deref(),
            Some("/api/agents/a%2Db")
        );
        // Canonical: a space and a non-ASCII byte must be escaped to travel, so
        // Go hands the handler the decoded text. This is the pair the raw path
        // got wrong.
        assert_eq!(
            route_path("/api/agents/a%20b").as_deref(),
            Some("/api/agents/a b")
        );
        assert_eq!(
            route_path("/api/agents/caf%C3%A9").as_deref(),
            Some("/api/agents/café")
        );
        // An encoded separator keeps the raw form, which is what stops it from
        // splitting into two segments and turning a one-segment route into a
        // miss.
        assert_eq!(
            route_path("/api/agents/a%2Fb").as_deref(),
            Some("/api/agents/a%2Fb")
        );
        // Nothing to decode: unchanged, and the overwhelmingly common case.
        assert_eq!(
            route_path("/api/agents/my-agent").as_deref(),
            Some("/api/agents/my-agent")
        );
    }

    #[test]
    fn a_malformed_escape_has_no_route_path() {
        // `ParseRequestURI` fails on each of these, so Go answers 400 from
        // inside `net/http`. `None` forwards, which is how that 400 is produced.
        assert_eq!(route_path("/api/agents/a%2"), None);
        assert_eq!(route_path("/api/agents/a%"), None);
        assert_eq!(route_path("/api/agents/a%zz"), None);
    }

    #[test]
    fn a_non_utf8_path_forwards_rather_than_being_mangled() {
        // Go routes `%FF` as a one-byte segment. Rust cannot carry it in a
        // `&str`, and a lossy conversion would look up a different slug — so
        // this forwards and Go answers.
        assert_eq!(route_path("/api/agents/%FF"), None);
        assert_eq!(
            unescape_path("/api/agents/%FF"),
            Some(b"/api/agents/\xff".to_vec())
        );
    }

    #[test]
    fn plus_is_not_a_space_in_a_path() {
        // `encodeQueryComponent`'s rule, not `encodePath`'s. If it leaked in,
        // every path holding a `+` would decode wrong *and* re-encode
        // differently, flipping the canonical check.
        assert_eq!(
            route_path("/api/agents/a+b").as_deref(),
            Some("/api/agents/a+b")
        );
        assert_eq!(escape_path("a b"), "a%20b");
    }

    #[test]
    fn escaping_is_uppercase_hex() {
        // Lowercase would make every escaped path look non-canonical, so
        // `route_path` would stop decoding the very cases it exists for.
        assert_eq!(escape_path("café"), "caf%C3%A9");
        assert_eq!(escape_path("a?b"), "a%3Fb");
    }

    #[test]
    fn the_reserved_bytes_a_path_may_carry_are_not_escaped() {
        // `?` is the only member of the reserved set `encodePath` escapes.
        assert_eq!(escape_path("$&+,/:;=@"), "$&+,/:;=@");
        assert_eq!(escape_path("-_.~"), "-_.~");
    }
}
