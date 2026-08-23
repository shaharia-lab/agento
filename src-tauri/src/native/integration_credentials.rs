//! Credential validation for `POST /api/integrations`.
//!
//! Mirrors `validateIntegrationCredentials` and the seven per-type validators
//! it dispatches to (`internal/service/integration_service.go`).
//!
//! # Why this is a module of its own
//!
//! It is the one part of the integration write path that is pure logic with no
//! SQL, and it is where the parity risk concentrates: seven types, each with its
//! own required fields and its own **exact** error strings, all of which ship to
//! the client as a 422 body.
//!
//! # Where the wording is this build's own
//!
//! Two of Go's failure paths embed an error string this port cannot reproduce,
//! and until #278 both forwarded so the sidecar could answer them exactly:
//!
//! - A credentials blob that is present but does not unmarshal. Go ships
//!   `invalid google credentials: ` plus `encoding/json`'s own message, which
//!   names Go types (`json: cannot unmarshal number into Go struct field
//!   GoogleCredentials.client_id of type string`). There is no Rust spelling of
//!   that.
//! - A `site_url` that `net/url` rejects outright, whose message comes from
//!   `url.Parse`.
//!
//! With the sidecar gone both are answered here as the **same 422 class** with
//! this build's own wording. Everything else — the far commoner "you left a
//! field blank" — is Go's text verbatim.
//!
//! # The three traps
//!
//! 1. **Empty and `null` are different.** Go tests `len(cfg.Credentials) == 0`,
//!    which is true only when the key is *absent*. A literal `"credentials":
//!    null` is four bytes, so it unmarshals to the zero value and reports the
//!    *field* error rather than "credentials are empty". Hence
//!    [`super::gojson::captured_raw`] on the request field, and hence
//!    `null_is_zero_value` on every string below — `{"client_id": null}` is `""`
//!    to Go and a type error to serde.
//! 2. **Jira rewrites what it stores.** It trims the trailing slash and calls
//!    `SetCredentials`, which re-marshals the *struct* — so unknown keys the
//!    caller sent are silently dropped and the three known ones are re-emitted
//!    in declaration order. Confluence does not do this, despite sharing the
//!    credential type. Reproduced in [`validate`]'s return value.
//! 3. **Confluence demands HTTPS; Jira accepts either.** Same struct, two
//!    different URL rules, and two different messages.

use serde::Deserialize;
use serde_json::value::RawValue;

use super::gojson::null_is_zero_value;
use super::writes::WriteError;

const FIELD_CREDENTIALS: &str = "credentials";
const FIELD_SITE_URL: &str = "credentials.site_url";
const FIELD_BOT_TOKEN: &str = "credentials.bot_token";

/// Validate an integration's credentials for its type.
///
/// `Ok(Some(json))` means the stored credentials must be **replaced** with
/// `json` — only Jira does this. `Ok(None)` means store what the caller sent,
/// byte for byte.
pub fn validate(
    integration_type: &str,
    credentials: Option<&RawValue>,
) -> Result<Option<String>, WriteError> {
    match integration_type {
        "google" => validate_google(credentials).map(|()| None),
        "confluence" => validate_confluence(credentials).map(|()| None),
        "telegram" => validate_telegram(credentials).map(|()| None),
        "jira" => validate_jira(credentials).map(Some),
        "github" => validate_github(credentials).map(|()| None),
        "slack" => validate_slack(credentials).map(|()| None),
        "whatsapp" => validate_whatsapp(credentials).map(|()| None),
        // Go's `default` arm: any other type just needs *something*. Note it
        // checks length only — it never looks at the content.
        _ => {
            if raw_is_absent(credentials) {
                return Err(WriteError::validation(
                    FIELD_CREDENTIALS,
                    "credentials are required",
                ));
            }
            Ok(None)
        }
    }
}

/// `len(cfg.Credentials) == 0` — absent, not `null`.
fn raw_is_absent(credentials: Option<&RawValue>) -> bool {
    credentials.map(|c| c.get().is_empty()).unwrap_or(true)
}

/// `cfg.ParseCredentials(&creds)`.
///
/// The empty case is a fixed string and so reproducible. A real unmarshal
/// failure is not, so it is reported as a 422 with this build's own wording
/// rather than the decoder's — which would quote the offending value.
fn parse<T>(kind: &str, credentials: Option<&RawValue>) -> Result<T, WriteError>
where
    T: for<'de> Deserialize<'de> + Default,
{
    let Some(raw) = credentials.filter(|c| !c.get().is_empty()) else {
        return Err(WriteError::validation(
            FIELD_CREDENTIALS,
            format!("invalid {kind} credentials: credentials are empty"),
        ));
    };
    // Two wrappers, one Go rule each, and the request struct beside this one
    // carries both on `services` for the same reasons (#295, #337):
    //
    // - `Option<T>` rather than straight to `T`, because a literal `null` is a
    //   *type error* to serde and the zero value to Go — a direct deserialize
    //   would refuse `"credentials": null` outright instead of reporting the
    //   first missing field, which is the inherited behaviour.
    // - `GoStruct<T>`, because serde fills a struct from a JSON **array**
    //   positionally when every field has a default, so `"credentials":["tok"]`
    //   decoded to a populated struct, passed the non-empty check and **created
    //   the row 201** where `validateTelegramCredentials` answers 422
    //   `cannot unmarshal array`. This is the decode behind all seven per-type
    //   validators, so it is the one place the rule decides whether a row is
    //   written at all rather than only whether a server is hosted.
    serde_json::from_str::<Option<super::gojson::GoStruct<T>>>(raw.get())
        .map(|wrapped| wrapped.map_or_else(T::default, |wrapped| wrapped.0))
        .map_err(|e| {
            // **The serde error text is deliberately not included.** serde_json
            // quotes the offending value, so a caller who sends
            // `"credentials": "ghp_realtoken"` would have that token echoed into
            // the message. Go's own error names the type and never the value.
            //
            // Go answered 422 with `encoding/json`'s wording here; that text is
            // not reproducible and the sidecar that used to supply it is gone
            // (#278), so this is the same 422 class with this build's own
            // wording. The position is enough to debug with and carries nothing
            // secret.
            WriteError::validation(
                FIELD_CREDENTIALS,
                format!(
                    "invalid {kind} credentials: malformed JSON at line {} column {}",
                    e.line(),
                    e.column()
                ),
            )
        })
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GoogleCredentials {
    #[serde(deserialize_with = "null_is_zero_value")]
    client_id: String,
    #[serde(deserialize_with = "null_is_zero_value")]
    client_secret: String,
}

fn validate_google(credentials: Option<&RawValue>) -> Result<(), WriteError> {
    let creds: GoogleCredentials = parse("google", credentials)?;
    if creds.client_id.is_empty() {
        return Err(WriteError::validation(
            "credentials.client_id",
            "client_id is required",
        ));
    }
    if creds.client_secret.is_empty() {
        return Err(WriteError::validation(
            "credentials.client_secret",
            "client_secret is required",
        ));
    }
    Ok(())
}

/// `config.AtlassianCredentials`, shared by Confluence and Jira.
#[derive(Default, Deserialize)]
#[serde(default)]
struct AtlassianCredentials {
    #[serde(deserialize_with = "null_is_zero_value")]
    site_url: String,
    #[serde(deserialize_with = "null_is_zero_value")]
    email: String,
    #[serde(deserialize_with = "null_is_zero_value")]
    api_token: String,
}

fn validate_confluence(credentials: Option<&RawValue>) -> Result<(), WriteError> {
    let creds: AtlassianCredentials = parse("confluence", credentials)?;
    if creds.site_url.is_empty() {
        return Err(WriteError::validation(
            FIELD_SITE_URL,
            "site_url is required",
        ));
    }
    // `confluence.ValidateSiteURL`: HTTPS only, and a hostname is required.
    let (scheme, host) = split_url(&creds.site_url)?;
    if scheme != "https" {
        return Err(WriteError::validation(
            FIELD_SITE_URL,
            // Go's `%q`. A scheme is ASCII, so this is a plain quoted string.
            format!("site URL must use HTTPS (got {scheme:?})"),
        ));
    }
    if host.is_empty() {
        return Err(WriteError::validation(
            FIELD_SITE_URL,
            "site URL must include a hostname",
        ));
    }
    atlassian_tail(&creds)
}

/// Jira's own URL rule, plus the normalization Confluence does not do.
fn validate_jira(credentials: Option<&RawValue>) -> Result<String, WriteError> {
    let mut creds: AtlassianCredentials = parse("jira", credentials)?;
    if creds.site_url.is_empty() {
        return Err(WriteError::validation(
            FIELD_SITE_URL,
            "site_url is required",
        ));
    }
    let (scheme, host) = split_url(&creds.site_url)?;
    if (scheme != "https" && scheme != "http") || host.is_empty() {
        return Err(WriteError::validation(
            FIELD_SITE_URL,
            "site_url must be a valid http or https URL",
        ));
    }
    // `strings.TrimRight(creds.SiteURL, "/")` — every trailing slash, not one.
    creds.site_url = creds.site_url.trim_end_matches('/').to_string();
    atlassian_tail(&creds)?;
    // `cfg.SetCredentials(creds)` re-marshals the struct: declaration order, no
    // `omitempty` on any of the three, and anything else the caller sent is
    // gone. This is the stored value from here on.
    Ok(format!(
        r#"{{"site_url":{},"email":{},"api_token":{}}}"#,
        json_string(&creds.site_url),
        json_string(&creds.email),
        json_string(&creds.api_token),
    ))
}

/// The email/api_token checks both Atlassian validators end with, in order.
fn atlassian_tail(creds: &AtlassianCredentials) -> Result<(), WriteError> {
    if creds.email.is_empty() {
        return Err(WriteError::validation(
            "credentials.email",
            "email is required",
        ));
    }
    if creds.api_token.is_empty() {
        return Err(WriteError::validation(
            "credentials.api_token",
            "api_token is required",
        ));
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct TelegramCredentials {
    #[serde(deserialize_with = "null_is_zero_value")]
    bot_token: String,
}

fn validate_telegram(credentials: Option<&RawValue>) -> Result<(), WriteError> {
    let creds: TelegramCredentials = parse("telegram", credentials)?;
    if creds.bot_token.is_empty() {
        return Err(WriteError::validation(
            FIELD_BOT_TOKEN,
            "bot_token is required",
        ));
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GitHubCredentials {
    #[serde(deserialize_with = "null_is_zero_value")]
    auth_mode: String,
    #[serde(deserialize_with = "null_is_zero_value")]
    personal_access_token: String,
}

fn validate_github(credentials: Option<&RawValue>) -> Result<(), WriteError> {
    let creds: GitHubCredentials = parse("github", credentials)?;
    // Note this rejects `oauth` and `app` even though the struct carries fields
    // for both — the upstream comment says only `pat` is wired up.
    if creds.auth_mode != "pat" {
        return Err(WriteError::validation(
            "credentials.auth_mode",
            "only 'pat' auth mode is currently supported",
        ));
    }
    if creds.personal_access_token.is_empty() {
        return Err(WriteError::validation(
            "credentials.personal_access_token",
            "personal_access_token is required",
        ));
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SlackCredentials {
    #[serde(deserialize_with = "null_is_zero_value")]
    auth_mode: String,
    #[serde(deserialize_with = "null_is_zero_value")]
    bot_token: String,
    #[serde(deserialize_with = "null_is_zero_value")]
    client_id: String,
    #[serde(deserialize_with = "null_is_zero_value")]
    client_secret: String,
}

fn validate_slack(credentials: Option<&RawValue>) -> Result<(), WriteError> {
    let creds: SlackCredentials = parse("slack", credentials)?;
    match creds.auth_mode.as_str() {
        "bot_token" => {
            if creds.bot_token.is_empty() {
                return Err(WriteError::validation(
                    FIELD_BOT_TOKEN,
                    "bot_token is required",
                ));
            }
        }
        "oauth" => {
            if creds.client_id.is_empty() {
                return Err(WriteError::validation(
                    "credentials.client_id",
                    "client_id is required",
                ));
            }
            if creds.client_secret.is_empty() {
                return Err(WriteError::validation(
                    "credentials.client_secret",
                    "client_secret is required",
                ));
            }
        }
        // Covers the empty string too, which is what an absent `auth_mode` is.
        _ => {
            return Err(WriteError::validation(
                "credentials.auth_mode",
                "auth_mode must be 'bot_token' or 'oauth'",
            ))
        }
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct WhatsAppCredentials {
    #[serde(deserialize_with = "null_is_zero_value")]
    #[allow(dead_code)] // Parsed only to reproduce Go's decode check.
    phone: String,
}

/// WhatsApp pairs by QR, so credentials are **optional** — absent is valid.
///
/// Kept despite the desktop app dropping WhatsApp (#273) for the same reason
/// `native/integrations.rs` still lists a `whatsapp` row: this is the API, not
/// the picker, and diverging from Go here would be a divergence for no gain.
fn validate_whatsapp(credentials: Option<&RawValue>) -> Result<(), WriteError> {
    if raw_is_absent(credentials) {
        return Ok(());
    }
    let _: WhatsAppCredentials = parse("whatsapp", credentials)?;
    Ok(())
}

/// Scheme and host as `net/url` would report them, or a 422 when the shape is
/// one only `url.Parse`'s own wording could describe.
///
/// Only the shapes an Atlassian site URL actually takes are decided here.
/// Anything carrying a space, a control character or a malformed `%` escape is
/// refused with this build's own wording — until #278 those forwarded so Go
/// could answer with `url.Parse`'s message, and the *class* (a 422 on
/// `credentials.site_url`, per `ValidationError`) is preserved even though the
/// text cannot be.
///
/// **This is the second of two places that reason about `net/url`'s rules, and
/// they answer different questions.** Here the question is *create*: may this
/// row be stored, so an uncertain input can be refused with a 422 the user
/// sees. In `native/integrations/confluence`, `validate_site_url` asks *start*:
/// may a stored row be hosted — where a refusal is silent, so it decides
/// everything itself and reproduces `getScheme` and the authority split
/// outright. They agree on every realistic input and are deliberately not
/// merged; #316 adds a third caller of the same Go rules with a *different*
/// answer again, since Jira does not require HTTPS.
fn split_url(raw: &str) -> Result<(String, String), WriteError> {
    // Answers a 422 with this build's own wording. It was called `forward`
    // when an `Err` here reached a second implementation; it never has since,
    // and the name outlived the mechanism.
    let refuse =
        || WriteError::validation("credentials.site_url", format!("invalid site URL {raw:?}"));
    if raw.chars().any(|c| c.is_control() || c == ' ') {
        return Err(refuse());
    }
    for (i, b) in raw.bytes().enumerate() {
        if b == b'%' {
            let rest = raw.as_bytes().get(i + 1..i + 3).unwrap_or(b"");
            if rest.len() != 2 || !rest.iter().all(|c| c.is_ascii_hexdigit()) {
                return Err(refuse());
            }
        }
    }

    // A leading `:` is `getScheme`'s own error (`missing protocol scheme`),
    // not an empty scheme, so it is refused rather than parsed.
    if raw.starts_with(':') {
        return Err(refuse());
    }

    // `url.Parse` takes the scheme only when what precedes the first ':' is a
    // valid scheme; otherwise the whole string is an opaque path and Scheme is
    // "". A bare `example.atlassian.net` therefore has no scheme and no host,
    // which is why it fails the HTTPS check rather than the hostname one.
    let Some((scheme, rest)) = raw.split_once(':') else {
        return Ok((String::new(), String::new()));
    };
    let valid_scheme = !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid_scheme {
        return Ok((String::new(), String::new()));
    }
    // Go lowercases the scheme.
    let scheme = scheme.to_ascii_lowercase();
    // Without `//` the remainder is opaque and Host stays empty.
    let Some(authority) = rest.strip_prefix("//") else {
        return Ok((scheme, String::new()));
    };
    let authority = authority.split(['/', '?', '#']).next().unwrap_or_default();

    // Everything below is a rule `parseHost` has that this port does not
    // reimplement — userinfo validation, IPv6 bracket matching, percent-escapes
    // (Go accepts some and rejects others) and port syntax. **Forwarding is the
    // only safe answer**, because the failure mode of guessing is not a wrong
    // message but a wrong *acceptance*: Rust would create an integration whose
    // `site_url` Go's own parser can never read, and being native the request
    // never reaches Go to be refused.
    if authority.contains(['@', '[', ']', '%']) {
        return Err(refuse());
    }
    // `parseHost` enforces an **allowlist**, not a blocklist, so this has to be
    // spelled the same way round. Enumerating every ASCII byte through
    // `url.Parse` in host position, Go rejects exactly `\`, `^`, `` ` ``, `{`,
    // `|` and `}` with `invalid character … in host name`, and accepts the rest
    // of the printable set — including `!"$&'()*+,-.;<=>_~`, which look
    // rejectable and are not. Bytes at or above 0x80 pass through unchanged, so
    // a non-ASCII host such as `exämple.net` is Go's to accept.
    const HOST_PUNCT: &[u8] = b":!\"$&'()*+,-.;<=>_~";
    if authority
        .bytes()
        .any(|b| b < 0x80 && !b.is_ascii_alphanumeric() && !HOST_PUNCT.contains(&b))
    {
        return Err(refuse());
    }
    // A `:` introduces a port, which Go requires to be digits.
    if let Some((_, port)) = authority.split_once(':') {
        if !port.chars().all(|c| c.is_ascii_digit()) {
            return Err(refuse());
        }
    }
    // The **whole authority**, port included: both Go validators test `u.Host`,
    // which keeps the port. Returning only the pre-colon part made
    // `https://:8080` — a valid `Host` of `:8080` to Go — look like no host at
    // all and answer 422. Emptiness is the only thing the callers test, so
    // including the port changes nothing else.
    Ok((scheme, authority.to_string()))
}

/// A Go-compatible JSON string literal.
///
/// `encoding/json` escapes `<`, `>` and `&` by default, which serde does not —
/// and these values are re-marshalled into the stored column, so the escaping
/// has to match or the bytes differ.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Go uses the shorthand for these two and `\u00XX` for the rest,
            // so the generic control-character arm below must not catch them.
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    /// A JSON **array** where a credentials object belongs.
    ///
    /// serde fills a struct from a sequence positionally when every field has a
    /// default, so this decoded to a populated struct, passed the non-empty check
    /// and created the row — where Go answers 422 `cannot unmarshal array`. It
    /// is a 422 here too since #278; only the wording is this build's own.
    ///
    /// The same rule `services` has carried since #337, one field along in the
    /// same request struct.
    #[test]
    fn a_json_array_is_not_a_credentials_object() {
        for (kind, blob) in [
            ("telegram", r#"["123456:AAF-token"]"#),
            ("github", r#"["pat","ghp_x"]"#),
            ("confluence", "[]"),
        ] {
            let raw = serde_json::value::RawValue::from_string(blob.to_string()).expect("raw");
            let err = validate(kind, Some(&raw)).expect_err("an array must not be accepted");
            assert!(
                matches!(err, WriteError::Validation { .. }),
                "{kind} {blob}: {err:?}"
            );
        }
    }

    use super::*;

    fn raw(s: &str) -> Box<RawValue> {
        RawValue::from_string(s.to_string()).expect("valid JSON")
    }

    fn err(integration_type: &str, creds: &str) -> WriteError {
        validate(integration_type, Some(&raw(creds))).expect_err("expected a validation failure")
    }

    fn message(integration_type: &str, creds: &str) -> String {
        err(integration_type, creds).message()
    }

    /// An **absent** blob is "credentials are empty"; a literal `null` is four
    /// bytes to Go, so it decodes to the zero value and reports the first
    /// missing field instead. Getting these the same way round is the whole
    /// reason the request field uses `captured_raw`.
    #[test]
    fn absent_credentials_and_null_credentials_fail_differently() {
        let absent = validate("google", None).expect_err("absent must fail");
        assert!(
            absent.message().contains("credentials are empty"),
            "{}",
            absent.message()
        );
        assert!(
            message("google", "null").contains("client_id is required"),
            "a literal null is a zero value to Go, not an absent blob"
        );
        // `{}` behaves like `null` for the same reason.
        assert!(message("google", "{}").contains("client_id is required"));
    }

    #[test]
    fn each_type_reports_its_own_first_missing_field() {
        assert!(message("google", r#"{"client_id":"a"}"#).contains("client_secret is required"));
        assert!(message("telegram", "{}").contains("bot_token is required"));
        assert!(message("github", r#"{"auth_mode":"oauth"}"#)
            .contains("only 'pat' auth mode is currently supported"));
        assert!(message("github", r#"{"auth_mode":"pat"}"#)
            .contains("personal_access_token is required"));
        assert!(message("slack", r#"{"auth_mode":"bot_token"}"#).contains("bot_token is required"));
        assert!(message("slack", r#"{"auth_mode":"oauth"}"#).contains("client_id is required"));
        assert!(message("slack", "{}").contains("auth_mode must be 'bot_token' or 'oauth'"));
        // An unknown type checks length only, never content.
        assert!(validate("madeup", Some(&raw(r#"{"anything":1}"#))).is_ok());
        assert!(validate("madeup", None)
            .expect_err("absent")
            .message()
            .contains("credentials are required"));
    }

    /// A JSON `null` in a string field is `""` to Go and a type error to serde.
    #[test]
    fn a_null_string_field_is_the_zero_value_not_an_error() {
        assert!(message("telegram", r#"{"bot_token":null}"#).contains("bot_token is required"));
    }

    /// Confluence is HTTPS-only and quotes the scheme it got; Jira takes either
    /// and has one message for every URL failure.
    #[test]
    fn the_two_atlassian_types_have_different_url_rules() {
        assert!(message("confluence", r#"{"site_url":"http://x.net"}"#)
            .contains(r#"site URL must use HTTPS (got "http")"#));
        // No scheme at all: Go reports an empty scheme, not a missing hostname.
        assert!(message("confluence", r#"{"site_url":"x.atlassian.net"}"#)
            .contains(r#"site URL must use HTTPS (got "")"#));
        assert!(message("confluence", r#"{"site_url":"https:"}"#)
            .contains("site URL must include a hostname"));

        // Jira accepts http.
        assert!(validate(
            "jira",
            Some(&raw(
                r#"{"site_url":"http://x.net","email":"e","api_token":"t"}"#
            ))
        )
        .is_ok());
        assert!(message("jira", r#"{"site_url":"ftp://x.net"}"#)
            .contains("site_url must be a valid http or https URL"));
    }

    /// Jira rewrites the stored blob; confluence stores what it was given.
    #[test]
    fn jira_normalises_and_reserialises_but_confluence_does_not() {
        let stored = validate(
            "jira",
            Some(&raw(
                r#"{"email":"e@x","extra":"dropped","site_url":"https://x.net///","api_token":"t"}"#,
            )),
        )
        .expect("valid")
        .expect("jira replaces the stored credentials");
        // Declaration order, every trailing slash gone, unknown key dropped.
        assert_eq!(
            stored,
            r#"{"site_url":"https://x.net","email":"e@x","api_token":"t"}"#
        );

        let unchanged = validate(
            "confluence",
            Some(&raw(
                r#"{"site_url":"https://x.net/","email":"e","api_token":"t"}"#,
            )),
        )
        .expect("valid");
        assert!(
            unchanged.is_none(),
            "confluence must store the caller's bytes verbatim, trailing slash and all"
        );
    }

    /// Go's `encoding/json` escapes these; the re-marshalled Jira blob has to
    /// match byte for byte.
    #[test]
    fn the_reserialised_blob_uses_gos_html_escaping() {
        let stored = validate(
            "jira",
            Some(&raw(
                r#"{"site_url":"https://x.net","email":"a&b<c>d","api_token":"t"}"#,
            )),
        )
        .expect("valid")
        .expect("replaced");
        assert_eq!(
            stored,
            r#"{"site_url":"https://x.net","email":"a\u0026b\u003cc\u003ed","api_token":"t"}"#,
            "serde would emit these three unescaped; encoding/json does not"
        );
    }

    /// WhatsApp is the one type where absent credentials are fine.
    #[test]
    fn whatsapp_allows_absent_credentials() {
        assert!(validate("whatsapp", None).is_ok());
        assert!(validate("whatsapp", Some(&raw(r#"{"phone":"+1"}"#))).is_ok());
    }

    /// A blob that does not decode is a 422 in this build's own words —
    /// Go's `encoding/json` wording is not reproducible, and since #278 there
    /// is no sidecar to supply it.
    #[test]
    fn an_undecodable_blob_is_a_422() {
        let e = err("google", r#"{"client_id":123}"#);
        assert!(
            matches!(e, WriteError::Validation { .. }),
            "a type mismatch must be a validation failure: {e:?}"
        );
    }

    /// Accepting a URL `url.Parse` would refuse would store a `site_url` the
    /// hosting code cannot read; each of these is refused as a 422 (Go's own
    /// class for site-url failures), with this build's wording since #278.
    #[test]
    fn every_url_shape_url_parse_would_refuse_is_refused_here() {
        // Each of these is an error from `url.Parse`, not a validation failure.
        for url in [
            "https://x.net:abc",   // invalid port
            "https://x.net:80:90", // two ports
            // Go rejects an escape whose first hex digit is below 8; it
            // *accepts* `%25` (host `x%net`) and valid multi-byte sequences
            // such as `%C3%A9` (host `xénet`). This port refuses all of them
            // rather than encoding that rule — deliberately, but do not
            // "correct" the guard toward "a host may never carry an escape",
            // which is not Go's rule.
            "https://ex%41mple.net",
            "https://[::1",         // unmatched bracket
            "https://a\\\\b@x.net", // invalid userinfo
            "https://a[b@x.net",    // invalid userinfo
            "://x.net",             // `missing protocol scheme`
        ] {
            let creds = format!(r#"{{"site_url":"{url}","email":"e","api_token":"t"}}"#);
            for kind in ["confluence", "jira"] {
                let e = validate(kind, Some(&raw(&creds))).expect_err("must not be accepted");
                assert!(
                    matches!(e, WriteError::Validation { .. }),
                    "{kind} {url:?} answered {:?} instead of a 422",
                    e.message()
                );
            }
        }
    }

    /// `parseHost` enforces an allowlist, so the guard has to be one too.
    ///
    /// These six are the *complete* set of ASCII characters Go rejects in a
    /// host. A guard built from shapes rather than from Go's own table missed
    /// every one of them — testing `split_url` directly keeps the JSON escaping
    /// of the outer credentials blob out of the way.
    #[test]
    fn the_six_characters_rejected_in_a_host_are_all_refused() {
        for bad in ['\\', '^', '`', '{', '|', '}'] {
            let url = format!("https://x.net{bad}a");
            assert!(
                split_url(&url).is_err(),
                "{url:?} was accepted, but url.Parse rejects it"
            );
        }
    }

    /// The other side of the allowlist: punctuation that *looks* rejectable but
    /// which is accepted must not be refused, or this over-rejects.
    #[test]
    fn the_punctuation_allowed_in_a_host_is_not_refused() {
        for ok in [
            '!', '"', '$', '&', '\'', '(', ')', '*', '+', ',', ';', '<', '=', '>', '_', '~',
        ] {
            let url = format!("https://x.net{ok}a");
            let (scheme, host) = split_url(&url)
                .unwrap_or_else(|e| panic!("{url:?} was refused, but it is valid: {e:?}"));
            assert_eq!(scheme, "https");
            assert_eq!(host, format!("x.net{ok}a"));
        }
        // Non-ASCII passes through unchanged in Go, so it must here too.
        let (_, host) = split_url("https://ex\u{e4}mple.net").expect("non-ascii host");
        assert_eq!(host, "ex\u{e4}mple.net");
    }

    /// Both Go validators test `u.Host`, which **includes the port** — so an
    /// empty hostname with a valid port is not an empty host.
    #[test]
    fn a_port_with_no_hostname_is_still_a_host() {
        let (_, host) = split_url("https://:8080").expect("valid to Go");
        assert_eq!(
            host, ":8080",
            "Go reads Host as \":8080\", which is not empty"
        );
        // …and the whole authority is the host, port included.
        let (_, host) = split_url("https://x.net:8443/wiki").expect("valid");
        assert_eq!(host, "x.net:8443");
    }

    /// …but an ordinary port is not a shape to be unsure of, and must not make
    /// the host look empty.
    #[test]
    fn a_numeric_port_is_understood_and_not_refused() {
        assert!(validate(
            "jira",
            Some(&raw(
                r#"{"site_url":"http://x.net:8080/","email":"e","api_token":"t"}"#
            ))
        )
        .is_ok());
        assert!(validate(
            "confluence",
            Some(&raw(
                r#"{"site_url":"https://x.net:8443","email":"e","api_token":"t"}"#
            ))
        )
        .is_ok());
    }

    /// `encoding/json` uses the two-character escapes for `0x08`/`0x0c` and
    /// `\u00XX` for every other control character.
    #[test]
    fn backspace_and_formfeed_use_gos_shorthand() {
        // `encoding/json` emits the two-character form for these two, and
        // `\u00XX` for the other control characters — so a single `< 0x20` arm
        // would be wrong for exactly this pair.
        assert_eq!(json_string("a\u{8}b"), r#""a\bb""#);
        assert_eq!(json_string("a\u{c}b"), r#""a\fb""#);
        assert_eq!(json_string("a\u{1}b"), r#""a\u0001b""#);
        // The ones serde also gets right, kept so the whole table is in one
        // place.
        assert_eq!(json_string("a\nb\tc\rd"), r#""a\nb\tc\rd""#);
    }

    /// The refusal reaches the wire and the log, so it must not carry the
    /// caller's bytes — serde_json's own message quotes the offending value.
    #[test]
    fn a_bad_credentials_blob_never_puts_the_secret_in_the_error() {
        let e = validate("google", Some(&raw(r#""ghp_SUPERSECRETVALUE""#)))
            .expect_err("a string is not a credentials object");
        assert!(matches!(e, WriteError::Validation { .. }));
        assert!(
            !e.message().contains("ghp_SUPERSECRETVALUE"),
            "the credential must not reach the log: {}",
            e.message()
        );
    }
}
