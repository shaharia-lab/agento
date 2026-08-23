//! Whether a **per-row API base** is one this build can send a request through
//! where Go would, and the prefix it contributes to every tool's path.
//!
//! Extracted from `native/integrations/confluence` (#317) when `jira` (#316)
//! needed the same answer. It is not general URL handling — it is one narrow
//! question with one right answer, arrived at over four rounds of review on
//! #317, and getting it wrong sent the user's `Basic` credentials to an
//! attacker's host. #313–#315 do not need it — their API bases are constants,
//! which #314 and #315 confirmed — and anything whose base comes out of the row
//! does.
//!
//! # The problem
//!
//! Go builds a request by concatenating the stored base with a path its tool
//! escaped, handing the string to `http.NewRequestWithContext`, and letting
//! `net/http` write `URL.RequestURI()` on the wire **verbatim**. `reqwest`
//! builds every request through `url::Url::parse`, which normalizes. The two
//! disagree in ways that reach a different endpoint, and for the *tool's* half
//! of the path `github::client::absolute` already handles it: compare what
//! `url` produced against what the tool built, and refuse rather than call
//! somewhere else.
//!
//! A per-row base breaks that in two ways, and the second is the interesting
//! one.
//!
//! ## The base is not necessarily encoded, so it cannot be compared raw
//!
//! GitHub's base is a fixed, already-escaped constant. A site URL is typed by a
//! person: `https://intranet.example.com/my atlassian` is one Go accepts and
//! sends as `/my%20atlassian/…` through `EscapedPath()`, and `url` encodes it
//! identically. Comparing the parsed target against the raw concatenation
//! therefore refuses every call against a base that works. So [`Base`] parses
//! the base **on its own** and uses its rendered path as the expected prefix;
//! only the tool's suffix is compared against the bytes the tool built, which is
//! sound because that half is fully `gourl`-escaped.
//!
//! ## Comparing two parsers of the same string cannot see an interpretation gap
//!
//! Once both sides go through `url`, any place `url` and `net/url` disagree
//! about the **base** is invisible: the comparison is the same parser on the
//! same bytes. It catches a disagreement about where the authority *ends* and
//! never one about what it *says* — and there are at least three of the latter,
//! each of which grafts the site onto an attacker's label from a string that
//! reads as the legitimate host:
//!
//! | base | `net/url` | `url` |
//! |---|---|---|
//! | `evil.com\@acme.atlassian.net` | `invalid userinfo` | host `evil.com` |
//! | `acme.atlassian.net%2Eevil.com` | `invalid URL escape` | `acme.atlassian.net.evil.com` |
//! | `acme.atlassian.net<U+00A0>evil.com` | that host, literally | IDNA-mapped and joined |
//!
//! `parseHost` is itself an **allowlist** — `integration_credentials::split_url`
//! says so, having enumerated every ASCII byte through it — so [`Base::new`]
//! uses one too, and a narrower one, because that module may refuse what it is
//! unsure of and a starter may not. See [`Mismatch::Authority`].
//!
//! # What each caller does with a [`Mismatch`], and why they differ
//!
//! The two callers answer differently **because Go does**, which is #277's
//! lesson repeated:
//!
//! - **Confluence** validates the site URL inside `Start` (HTTPS-only), so a
//!   base Go will not serve means Go hosts nothing. `validate_site_url` maps a
//!   [`Mismatch`] to a refusal, the registry logs it, and the integration is
//!   absent — which is what Go does.
//! - **Jira**'s `Start` validates nothing at all. Go hosts the server and
//!   advertises all nine tools whatever the base says. Refusing to host would
//!   change the *advertised tool set*, which is the surface an agent's stored
//!   `capabilities.mcp` allowlist depends on. So `jira::client::Client` holds
//!   the [`Mismatch`] instead and answers Go's own transport sentence per call:
//!   same tools, and no request built that this port cannot build faithfully.

use crate::native::gourl;

/// Why a base is one this build cannot send a request through.
///
/// A caller words its own message from this — the two callers reach a log line
/// and a tool result respectively, and neither text is Go's (Go has no
/// equivalent failure at all for most of these).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mismatch {
    /// The authority is not one the two parsers are guaranteed to read alike.
    ///
    /// An allowlist rather than a comparison, for the reason in the module
    /// header: ASCII letters, digits, `.`, `-` and `_`, with an optional
    /// `:`-separated numeric port, over the **whole** authority so that `@` and
    /// `\` are excluded — which is what makes the two agree on where the
    /// authority ends as well as on what it says.
    ///
    /// **What that buys, stated exactly:** no character in the set is
    /// *transformed* by either parser, so no admitted host can be disguised as
    /// another — the property all three gaps above attack. It is not quite "the
    /// two dial the same address": WHATWG runs its IPv4 parser on any host whose
    /// last label is numeric, so `0x7f.1`, `127.1`, `010.0.0.1` and
    /// `2130706433` are literal names to `net/url` and addresses to `url`. That
    /// is left standing deliberately — none of them can be passed off as
    /// `acme.atlassian.net`, and Go's own outcome is a failed lookup rather than
    /// a working host, so nothing is redirected *away* from a host Go was
    /// reaching. Refusing an all-numeric final label would close it, at the cost
    /// of hostnames shaped like `1.2.3.4.5`.
    ///
    /// It excludes four things Go serves: userinfo, an IPv6 literal, a
    /// non-ASCII host (which Go's own IDNA-blind resolver cannot dial either),
    /// and a percent escape.
    Authority,
    /// `url::Url::parse` will not parse it — an out-of-range port, say. Go's
    /// `url.Parse` refuses most of these too.
    Unparseable,
    /// It carries its own `?` or `#`, after which the concatenation is not a
    /// prefix of the target and there is no faithful answer.
    QueryOrFragment,
    /// Its path holds a malformed `%` escape. `setPath` decodes first and this
    /// is a parse error to Go as well.
    PathDoesNotDecode,
    /// `url` renders its path differently from Go's `EscapedPath()`.
    ///
    /// Go escapes `\ ^ |` in a path and `url` does not; `url` removes dot
    /// segments and `net/url` never does. Note the comparison is against
    /// **`EscapedPath()`** and not against `escape(Path, encodePath)`: the
    /// second is only the first's fallback, since Go prefers the raw text
    /// whenever it is [`gourl::valid_encoded_path`] — so `/a!b` and `/a%2Fb` are
    /// sent verbatim by Go even though `escape` would rewrite them, and
    /// comparing against `escape` alone refuses a base that works.
    PathEncoding,
}

/// A base URL both parsers read alike, and the path prefix `url` renders it as.
///
/// Built once — per `Start` for Confluence, per `Client` for Jira — because the
/// checks cost a percent-decode and a re-encode for a property of a value that
/// cannot change between calls.
#[derive(Clone)]
pub struct Base {
    /// The base exactly as stored. Concatenated with each tool's path, because
    /// that is what Go concatenates; nothing here is re-rendered.
    raw: String,
    /// What `url` puts in front of a tool's path. Empty when the base has no
    /// path of its own — `url` renders that as `/` and the tool's own leading
    /// slash supplies it.
    prefix: String,
}

impl Base {
    /// Checks `raw` and records the prefix it contributes, or says why not.
    ///
    /// `raw` is the base as stored, trailing slashes already trimmed by whatever
    /// Go trims them with (`ValidateSiteURL` for Confluence, the create-time
    /// validator for Jira).
    pub fn new(raw: &str) -> Result<Self, Mismatch> {
        // The authority first, ahead of even parsing the whole thing, because it
        // is the check that decides *which server* the credentials reach — so it
        // should be the answer whenever it applies, rather than whichever check
        // happens to notice first.
        let (_, rest) = go_scheme(raw);
        let authority = go_authority(rest);
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        let plain_host = !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        // An empty port is Go's `Host` of `acme.atlassian.net:`, which dials the
        // default on both sides; anything else must be digits, as
        // `validOptionalPort` requires.
        let plain_port = port.is_none_or(|port| port.bytes().all(|b| b.is_ascii_digit()));
        if !plain_host || !plain_port {
            return Err(Mismatch::Authority);
        }

        let parsed = reqwest::Url::parse(raw).map_err(|_| Mismatch::Unparseable)?;
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(Mismatch::QueryOrFragment);
        }

        let raw_path = base_path_of(raw);
        let go_path = if raw_path.is_empty() {
            "/".to_string()
        } else {
            // The `Option` alone: a well-formed escape decoding to bytes that
            // are not UTF-8 is not an error to Go — `%FF` is a perfectly good
            // path and `EscapedPath()` renders it back as `%FF` — which is why
            // `unescape_path` answers `Vec<u8>` and the fallback escapes bytes
            // rather than a `String`.
            let decoded = gourl::unescape_path(raw_path).ok_or(Mismatch::PathDoesNotDecode)?;
            if gourl::valid_encoded_path(raw_path) {
                raw_path.to_string()
            } else {
                gourl::escape_path_bytes(&decoded)
            }
        };
        if parsed.path() != go_path {
            return Err(Mismatch::PathEncoding);
        }

        Ok(Self {
            raw: raw.to_string(),
            // Whether the base contributed any path *text*, not what `url`
            // rendered — because `url` renders both `https://x` and `https://x/`
            // as `/` and Go does not treat them alike. Go concatenates, so
            // `https://x/` + `/rest/api/3/project` is `//rest/api/3/project` on
            // the wire, with the empty first segment intact. Deriving the prefix
            // from `parsed.path() == "/"` made that base refuse every call.
            //
            // Reachable on Jira and not on Confluence: `validate_site_url` trims
            // trailing slashes before it gets here, while `jira.Start` trims
            // nothing and `Update` validates nothing, so a user retyping the URL
            // in the edit form can store one.
            prefix: if raw_path.is_empty() {
                String::new()
            } else {
                parsed.path().to_string()
            },
        })
    }

    /// What this base puts in front of every tool's path — empty when it has no
    /// path of its own.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The URL a request to `path` goes to, or `None` when `url` would resolve
    /// it somewhere other than Go sends it.
    ///
    /// `path` is a tool's own, already escaped by `gourl` — `path_escape` per
    /// segment, `query_escape` per query value — which is what makes comparing
    /// it against `url`'s rendering sound: `encodePathSegment` escapes every byte
    /// in `url`'s path encode set and `encodeQueryComponent` every byte in its
    /// query one, so there is nothing left for `url` to percent-encode; existing
    /// `%XX` escapes keep their case, so Go's uppercase hex survives; and a path
    /// with no `?` compares `""` against `query()`'s `None`.
    ///
    /// The comparison is **exact** rather than a `..` scan, so anything else
    /// `url` normalizes in a tool's path — now or after an upgrade — is caught by
    /// construction. Host, scheme and port are not compared at all, so `url`'s
    /// default-port and case normalization cannot reach it.
    pub fn resolve(&self, path: &str) -> Option<reqwest::Url> {
        let url = reqwest::Url::parse(&format!("{}{path}", self.raw)).ok()?;
        let (want_path, want_query) = path.split_once('?').unwrap_or((path, ""));
        let want_path = format!("{}{want_path}", self.prefix);
        (url.path() == want_path && url.query().unwrap_or("") == want_query).then_some(url)
    }
}

/// `net/url`'s `getScheme`, lower-cased as `url.Parse` lower-cases it.
///
/// Answers `("", raw)` for anything without a valid scheme, which is what
/// `getScheme` does for every case except a leading `:` — and that one is a
/// parse error in Go, which a caller reaches by its own route.
pub fn go_scheme(raw: &str) -> (String, &str) {
    for (i, c) in raw.char_indices() {
        match c {
            'a'..='z' | 'A'..='Z' => {}
            '0'..='9' | '+' | '-' | '.' if i > 0 => {}
            ':' if i > 0 => return (raw[..i].to_ascii_lowercase(), &raw[i + 1..]),
            _ => return (String::new(), raw),
        }
    }
    (String::new(), raw)
}

/// The whole authority `url.Parse` would take, userinfo and port included —
/// everything between `//` and the first `/`, `?` or `#`.
///
/// Empty when there is no authority at all. Separate from [`go_host`] because
/// the two answer different questions: Go's own `u.Host == ""` wants the host,
/// and [`Base::new`]'s allowlist wants the userinfo too, since that is where a
/// `\` hides.
pub fn go_authority(rest: &str) -> &str {
    let Some(authority) = rest.strip_prefix("//") else {
        return "";
    };
    match authority.find(['/', '?', '#']) {
        Some(end) => &authority[..end],
        None => authority,
    }
}

/// The host `url.Parse` would set, given everything after the scheme's colon.
///
/// Empty unless an authority is present (`//…`), and empty for an authority that
/// is only userinfo — both of which Go answers with `u.Host == ""`.
pub fn go_host(rest: &str) -> &str {
    let authority = go_authority(rest);
    match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    }
}

/// The raw text of a base's path — everything from the `/` that ends the
/// authority.
///
/// Raw rather than parsed, because [`Mismatch::PathEncoding`] exists precisely
/// because parsing changes it.
pub fn base_path_of(raw: &str) -> &str {
    let Some(rest) = raw.split_once("://").map(|(_, rest)| rest) else {
        return "";
    };
    match rest.find('/') {
        Some(start) => &rest[start..],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal that matters most: a base where `url` and `net/url` read a
    /// **different host**, which is how a `Basic` credential leaves for somebody
    /// else's server. All three mechanisms, each verified against the real
    /// `net/url` when it was found.
    #[test]
    fn a_base_the_two_parsers_read_as_different_hosts_is_refused() {
        for raw in [
            // `url` treats `\` as an authority separator; `net/url` rejects the
            // userinfo that leaves, so Go hosts nothing.
            r"https://evil.com\@acme.atlassian.net",
            r"https://evil.com\@acme.atlassian.net/wiki",
            // `parseHost` rejects a `%` escape that decodes to a byte it would
            // have escaped; `url` decodes them all.
            "https://acme.atlassian.net%2Eevil.com",
            "https://acme.atlassian.net%41x.com",
            // A NO-BREAK SPACE: Go keeps it literally, `url` IDNA-maps it away
            // and joins the two labels into one name somebody else owns.
            "https://acme.atlassian.net\u{a0}evil.com",
            "https://exämple.net",
            // Refused with them, and Go serves all four.
            "https://user:pw@acme.atlassian.net",
            "https://[::1]:8443",
            "https://caf%C3%A9.example.com",
            "https://acme.atlassian.net:80x",
        ] {
            assert_eq!(Base::new(raw).err(), Some(Mismatch::Authority), "{raw}");
        }
    }

    /// …and the ordinary bases the allowlist must not catch, including the
    /// shapes a self-hosted deployment actually takes.
    #[test]
    fn an_ordinary_base_is_admitted() {
        for (raw, prefix) in [
            ("https://acme.atlassian.net", ""),
            ("https://ACME.Atlassian.NET", ""),
            ("https://acme.atlassian.net:8443", ""),
            ("https://confluence", ""),
            ("https://intranet_wiki.corp", ""),
            ("https://wiki-1.corp.example.com", ""),
            ("https://10.0.0.5:8090/confluence", "/confluence"),
            ("https://jira.corp.local/jira", "/jira"),
            // Not percent-encoded, and both parsers encode it the same way.
            (
                "https://intranet.example.com/my atlassian",
                "/my%20atlassian",
            ),
            ("https://intranet.example.com/café", "/caf%C3%A9"),
            ("https://intranet.example.com/a%20b", "/a%20b"),
            // `EscapedPath` sends these verbatim even though `escape` would
            // rewrite them, and `url` agrees.
            ("https://intranet.example.com/a!b(c)[d]", "/a!b(c)[d]"),
            ("https://intranet.example.com/a%2Fb", "/a%2Fb"),
            // A well-formed escape decoding to non-UTF-8 is a path Go serves.
            ("https://acme.atlassian.net/a%FFb", "/a%FFb"),
        ] {
            let base =
                Base::new(raw).unwrap_or_else(|e| panic!("{raw} is a base Go serves: {e:?}"));
            assert_eq!(base.prefix, prefix, "{raw}");
        }
    }

    /// A base ending in a bare `/` contributes one, and `url` cannot tell you so.
    ///
    /// `url::Url::parse` renders both `https://x` and `https://x/` with
    /// `path() == "/"`, but Go concatenates raw text: `https://x/` +
    /// `/rest/api/3/project` goes on the wire as `//rest/api/3/project`, empty
    /// first segment and all. Deriving the prefix from the *rendered* path made
    /// such a base refuse every call — silently, on Jira, where nothing trims it
    /// and nothing validates it on `PUT`.
    #[test]
    fn a_base_ending_in_a_slash_contributes_one() {
        let bare = Base::new("https://acme.atlassian.net").expect("no path");
        assert_eq!(bare.prefix(), "");
        assert_eq!(
            bare.resolve("/rest/api/3/project")
                .expect("resolves")
                .path(),
            "/rest/api/3/project"
        );

        let slashed = Base::new("https://acme.atlassian.net/").expect("a bare slash");
        assert_eq!(slashed.prefix(), "/");
        assert_eq!(
            slashed
                .resolve("/rest/api/3/project")
                .expect("resolves")
                .path(),
            "//rest/api/3/project",
            "Go sends the empty first segment, so this must too"
        );

        // …and the same one step down, which never had the bug because the
        // rendered path is not `/`.
        let nested = Base::new("https://acme.atlassian.net/jira/").expect("a trailing slash");
        assert_eq!(nested.prefix(), "/jira/");
        assert_eq!(
            nested
                .resolve("/rest/api/3/project")
                .expect("resolves")
                .path(),
            "/jira//rest/api/3/project"
        );
    }

    /// The path half of the disagreement, and the two shapes with no comparison
    /// at all.
    #[test]
    fn a_base_path_the_two_parsers_encode_differently_is_refused() {
        for raw in [
            r"https://acme.atlassian.net/a\b",
            "https://acme.atlassian.net/a^b",
            "https://acme.atlassian.net/a|b",
            "https://acme.atlassian.net/a/../b",
            "https://acme.atlassian.net/./a",
        ] {
            assert_eq!(Base::new(raw).err(), Some(Mismatch::PathEncoding), "{raw}");
        }
        assert_eq!(
            Base::new("https://acme.atlassian.net/a%zzb").err(),
            Some(Mismatch::PathDoesNotDecode)
        );
        for raw in [
            "https://acme.atlassian.net?a=b",
            "https://acme.atlassian.net#x",
        ] {
            assert_eq!(
                Base::new(raw).err(),
                Some(Mismatch::QueryOrFragment),
                "{raw}"
            );
        }
        // A port `url` cannot represent. Go accepts it and could never dial it.
        assert_eq!(
            Base::new("https://acme.atlassian.net:99999").err(),
            Some(Mismatch::Unparseable)
        );
    }

    /// [`Base::resolve`]: the dot-segment guard, and everything it must admit.
    #[test]
    fn resolve_refuses_a_dot_segment_and_nothing_legitimate() {
        let base = Base::new("https://acme.atlassian.net").expect("plain base");
        for path in [
            "/rest/api/3/issue/..",
            "/rest/api/3/issue/.",
            "/rest/api/3/issue/%2E%2E",
            "/rest/api/3/project/../../x",
        ] {
            assert!(base.resolve(path).is_none(), "{path}");
        }

        let ok = |path: &str| {
            base.resolve(path)
                .unwrap_or_else(|| panic!("{path} is a legitimate path"))
        };
        assert_eq!(ok("/rest/api/3/project").query(), None);
        assert_eq!(
            ok("/wiki/api/v2/spaces?limit=50").path(),
            "/wiki/api/v2/spaces"
        );
        assert_eq!(
            ok("/wiki/api/v2/search?cql=space+%3D+DEV&limit=25").query(),
            Some("cql=space+%3D+DEV&limit=25")
        );
        assert_eq!(
            ok("/rest/api/3/issue/my%20key%2Fx").path(),
            "/rest/api/3/issue/my%20key%2Fx",
            "uppercase hex survives, and so does an escaped slash"
        );
        // A dot *inside* a segment is not a dot segment, and neither is one in a
        // query value — only the path is resolved.
        assert_eq!(
            ok("/rest/api/3/issue/v1.2.3").path(),
            "/rest/api/3/issue/v1.2.3"
        );
        assert_eq!(
            ok("/wiki/api/v2/search?cql=..%2F..&limit=25").query(),
            Some("cql=..%2F..&limit=25")
        );
    }

    /// …and the guard still fires behind a base with a path of its own, because
    /// only the tool's suffix is compared against the bytes it built.
    #[test]
    fn resolve_still_fires_behind_a_base_path() {
        for raw in [
            "https://jira.corp.local/jira",
            "https://intranet.example.com/my atlassian",
        ] {
            let base = Base::new(raw).expect("a base Go serves");
            assert!(base.resolve("/rest/api/3/issue/..").is_none(), "{raw}");
            let url = base
                .resolve("/rest/api/3/project")
                .unwrap_or_else(|| panic!("{raw}"));
            assert!(
                url.path().ends_with("/rest/api/3/project"),
                "{raw}: {}",
                url.path()
            );
            assert!(url.path().starts_with(base.prefix()), "{raw}");
        }
    }

    #[test]
    fn the_net_url_transcriptions_answer_what_net_url_answers() {
        for (raw, scheme) in [
            ("https://acme.atlassian.net", "https"),
            ("HTTPS://acme.atlassian.net", "https"),
            ("http://acme.atlassian.net", "http"),
            ("h2.c+x-y://acme", "h2.c+x-y"),
            ("acme.atlassian.net", ""),
            ("1https://acme", ""),
            ("ht tps://acme", ""),
        ] {
            assert_eq!(go_scheme(raw).0, scheme, "{raw}");
        }
        assert_eq!(go_host("//acme.atlassian.net/wiki"), "acme.atlassian.net");
        assert_eq!(go_host("//user:pw@acme:8443?x"), "acme:8443");
        assert_eq!(go_authority("//user:pw@acme:8443?x"), "user:pw@acme:8443");
        for rest in ["", "//", "//user@", "opaque", "/path"] {
            assert!(go_host(rest).is_empty(), "{rest}");
        }
        assert_eq!(base_path_of("https://acme.atlassian.net"), "");
        assert_eq!(base_path_of("https://acme.atlassian.net/a/b"), "/a/b");
    }
}
