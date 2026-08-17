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

/// Which of `net/url`'s escaping modes a byte is being judged under.
///
/// Go's `shouldEscape` takes this as a parameter and the three arms genuinely
/// differ — which is the whole reason this enum exists rather than one
/// function. `escape_path` was the only mode until #312; the GitHub integration
/// needs the other two, and reaching for the wrong one is invisible in a
/// response.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    /// `encodePath` — a whole path. What [`route_path`]'s canonical check runs
    /// under.
    Path,
    /// `encodePathSegment` — what `url.PathEscape` uses, and what every
    /// `url.PathEscape(p.Owner)` in `internal/integrations/` is.
    PathSegment,
    /// `encodeQueryComponent` — what `url.QueryEscape` uses, and therefore what
    /// `url.Values.Encode` applies to every key and value.
    QueryComponent,
}

/// Bytes Go's `escape(s, mode)` leaves alone.
///
/// Transcribed from `shouldEscape` in `net/url/url.go`: alphanumerics and the
/// unreserved marks `-_.~` are never escaped in any mode, everything else —
/// every byte over 0x7F included — always is, and the reserved set
/// `$ & + , / : ; = ? @` is where the modes part company:
///
/// | mode | escapes, of the reserved set |
/// |---|---|
/// | `encodePath` | `?` |
/// | `encodePathSegment` | `/ ; , ?` |
/// | `encodeQueryComponent` | all of them |
///
/// The two that bite: a **query** escapes `~`'s neighbours but not `~` itself
/// while escaping `*` (`form_urlencoded`, already in this tree, does the exact
/// reverse), and a **path segment** escapes `/` where a whole path does not —
/// which is what stops a repository named `a/b` from becoming two segments.
fn should_escape(c: u8, mode: Encoding) -> bool {
    if c.is_ascii_alphanumeric() {
        return false;
    }
    match c {
        b'-' | b'_' | b'.' | b'~' => false,
        // The RFC allows `: @ & = + $` in a path and reserves `/ ; ,` for
        // assigning meaning to individual segments; `net/url` manipulates the
        // path as a whole and so allows those three too. That leaves `?`.
        b'$' | b'&' | b'+' | b',' | b'/' | b':' | b';' | b'=' | b'?' | b'@' => match mode {
            Encoding::Path => c == b'?',
            Encoding::PathSegment => matches!(c, b'/' | b';' | b',' | b'?'),
            // "The RFC reserves (so we must escape) everything."
            Encoding::QueryComponent => true,
        },
        _ => true,
    }
}

/// Go's `escape(s, encodePath)`.
///
/// Uppercase hex, and **no `+` for space** — that substitution belongs to
/// `encodeQueryComponent`, and using it here would make every path holding a
/// space look non-canonical and flip [`route_path`] to the wrong branch.
pub fn escape_path(s: &str) -> String {
    escape_bytes(s.as_bytes(), Encoding::Path)
}

/// Go's `validEncoded(s, encodePath)` — whether `net/url` would send `s` as it
/// stands rather than re-escaping it.
///
/// This is the half of `URL.EscapedPath()` that is easy to miss, and missing it
/// makes a faithful-looking comparison wrong. `EscapedPath` does **not** simply
/// return `escape(Path, encodePath)`: when the raw text differs from that and is
/// *validly encoded*, the raw text wins. And `validEncoded`'s allowlist is wider
/// than `should_escape`'s — it admits `! $ & ' ( ) * + , ; = : @ [ ] %`
/// unconditionally, "not specified in RFC 3986 but left alone by modern
/// browsers".
///
/// So `/a!b` is sent verbatim even though `escape` would render it `/a%21b`,
/// and `/a%2Fb` is sent verbatim even though its decoded form re-escapes to
/// `/a/b`. A caller comparing another parser's output against `escape` alone
/// would call both of those a divergence when they are not one.
///
/// The one caller today is `native/integrations/confluence`, which uses it to
/// decide whether a stored site URL is one this build can send where Go sends
/// it; #316 needs the same question answered for Jira.
pub fn valid_encoded_path(s: &str) -> bool {
    s.bytes().all(|c| match c {
        b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b':'
        | b'@' => true,
        // "not specified in RFC 3986 but left alone by modern browsers"
        b'[' | b']' => true,
        // Percent-encoded, and `unescape` has the job of deciding whether it
        // decodes at all.
        b'%' => true,
        _ => !should_escape(c, Encoding::Path),
    })
}

/// [`escape_path`] over raw bytes.
///
/// A Go string is arbitrary bytes and `escape` is defined over them, so a
/// caller that has just come out of [`unescape_path`] — which answers
/// `Vec<u8>` for exactly that reason — must not have to demand UTF-8 first.
/// `%FF` is a perfectly good path to Go, and `EscapedPath()` renders it back as
/// `%FF`; requiring UTF-8 in between would refuse it.
pub fn escape_path_bytes(bytes: &[u8]) -> String {
    escape_bytes(bytes, Encoding::Path)
}

/// Go's `url.PathEscape` — `escape(s, encodePathSegment)`.
///
/// One segment, so `/` is escaped: this is what every
/// `fmt.Sprintf("/repos/%s/%s", url.PathEscape(owner), url.PathEscape(repo))`
/// in `internal/integrations/` relies on, and a repository literally named
/// `a/b` is the case that shows it.
pub fn path_escape(s: &str) -> String {
    escape_bytes(s.as_bytes(), Encoding::PathSegment)
}

/// Go's `url.QueryEscape` — `escape(s, encodeQueryComponent)`, space included.
///
/// The space rule lives in `escape` rather than in `shouldEscape`: a space
/// becomes `+`, not `%20`. That is the one byte where this and
/// percent-encoding-by-the-RFC visibly disagree, and `url.Values.Encode` puts
/// it in every search query a user types.
pub fn query_escape(s: &str) -> String {
    escape_bytes(s.as_bytes(), Encoding::QueryComponent)
}

/// The same function over bytes, which is what [`route_path`] needs.
///
/// A Go string is arbitrary bytes and `escape` is defined over them, so the
/// canonical check has to run before anything demands UTF-8 — see
/// [`route_path`]. The output is ASCII either way: every byte over 0x7F is
/// escaped, so `char` is safe here for exactly the reason `should_escape` says.
fn escape_bytes(bytes: &[u8], mode: Encoding) -> String {
    const UPPERHEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len());
    for &c in bytes {
        // Go tests this *before* `shouldEscape`, and only in this mode: a space
        // is `+` in a query component and `%20` everywhere else.
        if c == b' ' && mode == Encoding::QueryComponent {
            out.push('+');
        } else if should_escape(c, mode) {
            out.push('%');
            out.push(UPPERHEX[(c >> 4) as usize] as char);
            out.push(UPPERHEX[(c & 15) as usize] as char);
        } else {
            out.push(c as char);
        }
    }
    out
}

/// Go's `url.Values` — a `map[string][]string`, sorted by key on encode.
///
/// It was a single value per key until #313, because every construction in
/// `internal/integrations/` was a sequence of `q.Set(k, v)` and that models
/// `Set`'s replace exactly. Google's generated clients broke the assumption:
/// `search_email` sends `metadataHeaders` three times, and measured against the
/// real library the result is `…&metadataHeaders=Subject&metadataHeaders=From&
/// metadataHeaders=Date&…` — keys sorted, **values in insertion order**, which is
/// what `Encode` does and what a single-valued map cannot express.
///
/// The sorting is not cosmetic. It is what makes a request reproducible, which
/// is what lets `desktop/parity/github_vectors.json` pin the encoded target of
/// every paged tool.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Values(std::collections::BTreeMap<String, Vec<String>>);

impl Values {
    /// An empty set of parameters — `url.Values{}`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `Values.Set`: replaces whatever was under `key`, however many values it
    /// held.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), vec![value.into()]);
    }

    /// `Values.Add`: appends, keeping insertion order within the key.
    ///
    /// One caller — Google's `search_email` — and it is the reason this type
    /// stopped being single-valued. See the type's own docs.
    pub fn add(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.entry(key.into()).or_default().push(value.into());
    }

    /// `Values.Encode`: `k=v` pairs joined by `&`, keys sorted, values in the
    /// order they were added, both halves through [`query_escape`].
    ///
    /// An empty set encodes to `""` — which the callers rely on, since they all
    /// append it after a literal `?` and Go produces a bare trailing `?` in
    /// exactly the same case.
    pub fn encode(&self) -> String {
        let mut out = String::new();
        for (key, values) in &self.0 {
            let key = query_escape(key);
            for value in values {
                if !out.is_empty() {
                    out.push('&');
                }
                out.push_str(&key);
                out.push('=');
                out.push_str(&query_escape(value));
            }
        }
        out
    }
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
/// - the escaping is canonical **and** the decoded path is not UTF-8
///   (`/api/agents/%FF`). Go carries it happily; Rust cannot put it in a `&str`,
///   and every claim function and bound parameter downstream wants one. Rather
///   than lossily converting — which would look up a *different* slug than Go
///   looks up — the request forwards and Go answers it correctly.
///
/// **The canonical check runs on bytes, before anything demands UTF-8**, and the
/// order is load-bearing: `/api/agents/%ff` decodes to the same unrepresentable
/// byte as `/api/agents/%FF`, but its escaping is *not* canonical, so chi routes
/// on the raw target — which is plain ASCII and perfectly carryable. Demanding
/// UTF-8 first would forfeit that route path for no reason, and would make the
/// case impossible to add to the vectors.
pub fn route_path(raw: &str) -> Option<String> {
    let decoded = unescape_path(raw)?;
    // `url.setPath`: the escaping is canonical exactly when re-encoding the
    // decoded path reproduces what arrived, and that is when `RawPath` stays
    // empty and chi falls through to the decoded `URL.Path`.
    if escape_bytes(&decoded, Encoding::Path) == raw {
        String::from_utf8(decoded).ok()
    } else {
        // Non-canonical: chi routes on `RawPath`, which is the request target
        // this was called with and therefore already a `&str`.
        Some(raw.to_string())
    }
}

#[cfg(test)]
mod tests {
    /// `Encode` sorts keys and keeps each key's values in insertion order —
    /// measured against the real `url.Values` through Google's generated client,
    /// which sends `metadataHeaders` three times.
    #[test]
    fn repeated_values_keep_their_order_under_a_sorted_key() {
        let mut values = Values::new();
        values.set("format", "metadata");
        values.add("metadataHeaders", "Subject");
        values.add("metadataHeaders", "From");
        values.add("metadataHeaders", "Date");
        values.set("alt", "json");
        values.set("prettyPrint", "false");
        assert_eq!(
            values.encode(),
            "alt=json&format=metadata&metadataHeaders=Subject&metadataHeaders=From\
             &metadataHeaders=Date&prettyPrint=false"
                .replace(char::is_whitespace, "")
        );

        // `set` still replaces, however many values were there.
        let mut values = Values::new();
        values.add("k", "a");
        values.add("k", "b");
        values.set("k", "c");
        assert_eq!(values.encode(), "k=c");
    }

    /// `validEncoded`'s allowlist is wider than `should_escape`'s, and the gap
    /// is the whole reason this function exists rather than a caller comparing
    /// against [`escape_path`]. Every byte here is one Go sends **verbatim**
    /// while `escape` would rewrite it.
    /// [`escape_path_bytes`] is [`escape_path`] without the UTF-8 demand, and
    /// the bytes that show it are the ones a `String` cannot hold.
    #[test]
    fn escaping_bytes_does_not_require_utf8() {
        assert_eq!(escape_path_bytes(b"/a/b"), "/a/b");
        assert_eq!(escape_path_bytes("/café".as_bytes()), "/caf%C3%A9");
        // Not valid UTF-8, and a path Go both accepts and renders back.
        assert_eq!(escape_path_bytes(b"/a\xffb"), "/a%FFb");
        assert_eq!(escape_path_bytes(b"/\xff\xfe"), "/%FF%FE");
    }

    #[test]
    fn valid_encoded_admits_what_escape_would_rewrite() {
        for raw in [
            "/a!b",
            "/a(b)c",
            "/a[b]",
            "/a'b",
            "/a*b",
            "/a%2Fb",
            "/a/../b",
            "/a:b@c",
            "/a$b&c",
            "/a+b,c;d=e",
            "/plain",
            "",
        ] {
            assert!(valid_encoded_path(raw), "{raw}");
        }
        // …and the bytes it refuses, which are the ones `EscapedPath` re-escapes.
        for raw in [
            "/a b", "/a\\b", "/a^b", "/a|b", "/a\"b", "/a<b>c", "/a`b", "/a{b}", "/a#b", "/a?b",
            "/café",
        ] {
            assert!(!valid_encoded_path(raw), "{raw}");
        }
    }

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
        path_escape: Vec<EscapeCase>,
        query_escape: Vec<EscapeCase>,
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

    /// The two modes every ported integration builds a request with.
    #[test]
    fn path_and_query_escaping_match_the_go_vectors() {
        let v = vectors();
        assert!(v.path_escape.len() >= 20, "vectors look truncated");
        for case in v.path_escape {
            assert_eq!(
                path_escape(&case.value),
                case.want,
                "url.PathEscape({:?})",
                case.value
            );
        }
        for case in v.query_escape {
            assert_eq!(
                query_escape(&case.value),
                case.want,
                "url.QueryEscape({:?})",
                case.value
            );
        }
    }

    /// The three bytes a reader is most likely to assume, spelled out here so
    /// the rule is legible without opening the vectors — and because each is a
    /// place an off-the-shelf encoder disagrees with Go.
    #[test]
    fn the_two_modes_disagree_exactly_where_go_says_they_do() {
        // A segment escapes `/`; a whole path does not. This is what keeps a
        // repository named `a/b` one segment.
        assert_eq!(path_escape("a/b"), "a%2Fb");
        assert_eq!(escape_path("a/b"), "a/b");
        // A space is `+` in a query and `%20` in a segment.
        assert_eq!(query_escape("a b"), "a+b");
        assert_eq!(path_escape("a b"), "a%20b");
        // `~` survives and `*` does not — `form_urlencoded` does the reverse.
        assert_eq!(query_escape("~a*b"), "~a%2Ab");
    }

    /// `Values.Encode` sorts by key and escapes both halves, which is what
    /// makes every paged GitHub request reproducible.
    #[test]
    fn values_encode_sorts_keys_and_escapes_both_halves() {
        let mut values = Values::new();
        values.set("per_page", "30");
        values.set("q", "repo:o/r func foo");
        values.set("labels", "bug,help wanted");
        assert_eq!(
            values.encode(),
            "labels=bug%2Chelp+wanted&per_page=30&q=repo%3Ao%2Fr+func+foo"
        );

        // `Set` replaces, and an empty set is the empty string — the callers
        // append it after a literal `?` either way.
        values.set("per_page", "100");
        assert!(values.encode().contains("per_page=100"));
        assert_eq!(Values::new().encode(), "");
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
    fn a_non_utf8_path_forwards_only_when_that_is_what_chi_routes_on() {
        // `%FF` is canonical, so chi routes on the *decoded* path: one byte Rust
        // cannot carry in a `&str`, and a lossy conversion would look up a
        // different slug. Forwarding is the only right answer.
        assert_eq!(route_path("/api/agents/%FF"), None);
        assert_eq!(
            unescape_path("/api/agents/%FF"),
            Some(b"/api/agents/\xff".to_vec())
        );

        // `%ff` decodes to the same byte and is *not* canonical, so chi routes
        // on the raw target — plain ASCII, and perfectly carryable. Testing
        // UTF-8 before the canonical check would forfeit this one for nothing.
        assert_eq!(
            route_path("/api/agents/%ff").as_deref(),
            Some("/api/agents/%ff")
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
