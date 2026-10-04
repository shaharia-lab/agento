//! The vendored credential rule table.
//!
//! **The rule shapes are gitleaks'** (`config/gitleaks.toml`, Apache-2.0),
//! re-expressed for Rust's `regex` crate rather than written from scratch: that
//! list has been tuned against real leaks for years, and v1's goal is a *few*
//! findings that are almost never wrong. Only rules whose match format has no
//! plausible non-secret reading ship here — no entropy heuristics, no generic
//! `password=` scanning (design doc §4.1). Where a pattern departs from
//! gitleaks, the rule says why.
//!
//! Two properties every pattern keeps:
//!
//! - **Bounded in cost.** The `regex` crate guarantees linear-time search — it
//!   has no backtracking engine — so an adversarial transcript cannot make a
//!   rule catastrophically slow. The price is that there are no look-arounds:
//!   where a rule needs to know what surrounds a match, its `accept` check
//!   says so instead.
//! - **One tier per rule, fixed.** The confidence a user sees is the rule's
//!   tier plus its id, so a finding always traces back to the pattern that
//!   fired (§4.2). The JWT rule is the only `medium` one.
//!
//! Upstream key formats drift (providers rotate prefixes); keeping this table
//! in step with gitleaks is manual, and every change to it bumps
//! [`CURRENT_RULESET_VERSION`].

use std::sync::OnceLock;

use regex::Regex;

use super::store;

/// Bumped whenever a rule is added, removed or changed; sessions scanned under
/// an older version are rescanned. The same contract as
/// `insights::processors::CURRENT_PROCESSOR_VERSION`.
///
/// 2 (#741): no rule changed, the *stored form* did — a database URL's masked
/// snippet no longer keeps the end of the password when the host is short, and
/// only a rescan rewrites a snippet stored under 1.
pub const CURRENT_RULESET_VERSION: i64 = 2;

/// How sure a rule is that a match is a live secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
}

impl Confidence {
    /// The stored spelling, `'high' | 'medium'` (design doc §5.2).
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
        }
    }
}

/// One detection rule.
#[derive(Clone, Copy)]
pub struct Rule {
    /// Stable id, stored on every finding and used by rule-level whitelisting.
    /// Renaming one orphans both — treat it as a wire value.
    pub id: &'static str,
    pub provider: &'static str,
    pub confidence: Confidence,
    pattern: &'static str,
    /// A second test on the reported text, for what a regex cannot say.
    pub(super) accept: Option<fn(&str) -> bool>,
    /// Only credited within [`PAIR_WINDOW`] bytes of a finding of this rule.
    pub(super) paired_with: Option<&'static str>,
    /// The rule's own display form, for a match whose last four characters are
    /// not safe to show; `None` is [`store::mask`]. Reached only through
    /// [`store::mask_for`], which has already hidden a short match whole.
    pub(super) mask: Option<fn(&str) -> String>,
}

/// How close, in bytes, a paired rule's match must sit to its partner's.
pub(super) const PAIR_WINDOW: usize = 256;

const RULES: &[Rule] = &[
    Rule {
        id: "aws-access-key-id",
        provider: "AWS",
        confidence: Confidence::High,
        pattern: r"\bAKIA[0-9A-Z]{16}\b",
        accept: None,
        paired_with: None,
        mask: None,
    },
    // Forty base64-ish characters are also a SHA-1 in hex, a git object id, a
    // chunk of any base64 blob — so this one is never credited on its own
    // (gitleaks needs an `aws … secret` keyword for the same reason). It counts
    // only beside an access-key-id, and only when it mixes cases and digits the
    // way a generated key does; a hex digest never has an upper-case letter.
    //
    // The pattern takes a whole run and `aws_secret_shape` keeps runs of
    // exactly forty: with no look-around, bounding the run by consuming a
    // delimiter would miss `KEY=<secret>` and a candidate one space after
    // another. `=` is outside the run so `AWS_SECRET_ACCESS_KEY=` does not
    // join it, and trails it so a padded base64 blob is refused whole.
    Rule {
        id: "aws-secret-access-key",
        provider: "AWS",
        confidence: Confidence::High,
        pattern: r"[A-Za-z0-9/+]{40,}={0,2}",
        accept: Some(aws_secret_shape),
        paired_with: Some("aws-access-key-id"),
        mask: None,
    },
    // The `private_key` member of a service-account key file. Matches both the
    // JSON-escaped form (`\n`) a transcript holds and a decoded one.
    Rule {
        id: "gcp-service-account-key",
        provider: "Google Cloud",
        confidence: Confidence::High,
        pattern: r#""private_key"\s*:\s*"-----BEGIN PRIVATE KEY-----[^"]{64,}?-----END PRIVATE KEY-----(?:\\n|\n)?""#,
        accept: None,
        paired_with: None,
        mask: None,
    },
    // Storage (`AccountKey=`) and Service Bus / Event Hubs (`SharedAccessKey=`)
    // connection strings. The key is what leaks, so the key is what matches.
    Rule {
        id: "azure-connection-string",
        provider: "Azure",
        confidence: Confidence::High,
        pattern: r"\b(?:AccountKey|SharedAccessKey)=[A-Za-z0-9+/]{40,}={0,2}",
        accept: None,
        paired_with: None,
        mask: None,
    },
    Rule {
        id: "github-pat",
        provider: "GitHub",
        confidence: Confidence::High,
        pattern: r"\bghp_[A-Za-z0-9]{36}\b",
        accept: None,
        paired_with: None,
        mask: None,
    },
    Rule {
        id: "github-oauth",
        provider: "GitHub",
        confidence: Confidence::High,
        pattern: r"\bgho_[A-Za-z0-9]{36}\b",
        accept: None,
        paired_with: None,
        mask: None,
    },
    Rule {
        id: "github-fine-grained-pat",
        provider: "GitHub",
        confidence: Confidence::High,
        pattern: r"\bgithub_pat_[A-Za-z0-9_]{82}\b",
        accept: None,
        paired_with: None,
        mask: None,
    },
    // Digits after the prefix, unlike a bare `xox[baprs]-.+`: a real token is
    // `xoxb-<team>-<bot>-<secret>`, and a docs placeholder such as
    // `xoxb-your-token-here` is the false positive the looser shape invites.
    Rule {
        id: "slack-token",
        provider: "Slack",
        confidence: Confidence::High,
        pattern: r"\bxox[baprs]-[0-9]{8,}-[A-Za-z0-9-]{8,}",
        accept: None,
        paired_with: None,
        mask: None,
    },
    Rule {
        id: "stripe-live-key",
        provider: "Stripe",
        confidence: Confidence::High,
        pattern: r"\b(?:sk|rk)_live_[A-Za-z0-9]{24,}\b",
        accept: None,
        paired_with: None,
        mask: None,
    },
    // The legacy `sk-` + alphanumerics shape, and the project/service-account/
    // admin keys OpenAI issues now, whose body also carries `_` and `-`.
    // Anthropic's `sk-ant-…` cannot match either: `ant` is followed by `-`.
    Rule {
        id: "openai-api-key",
        provider: "OpenAI",
        confidence: Confidence::High,
        pattern: r"\bsk-(?:[A-Za-z0-9]{20,}|(?:proj|svcacct|admin)-[A-Za-z0-9_-]{40,})",
        accept: None,
        paired_with: None,
        mask: None,
    },
    // `sk-ant-api03-…`, `sk-ant-admin01-…`, `sk-ant-oat01-…`: a kind and a
    // two-digit version before the body.
    Rule {
        id: "anthropic-api-key",
        provider: "Anthropic",
        confidence: Confidence::High,
        pattern: r"\bsk-ant-[a-z]+[0-9]{2}-[A-Za-z0-9_-]{20,}",
        accept: None,
        paired_with: None,
        mask: None,
    },
    Rule {
        id: "npm-access-token",
        provider: "npm",
        confidence: Confidence::High,
        pattern: r"\bnpm_[A-Za-z0-9]{36}\b",
        accept: None,
        paired_with: None,
        mask: None,
    },
    // `scheme://user:password@host`. A template (`${DB_PASSWORD}`,
    // `<password>`) cannot match the password class. `%` can, because a
    // password holding `@`, `/` or `:` only travels percent-encoded; so
    // `real_db_password` credits a `%` only as an escape (`%40`), refusing
    // format strings (`%s`, `%(pw)s`) and batch variables (`%VAR%`), along with
    // the placeholder words docs and compose files use.
    //
    // The match is the whole URL up to the end of the host, so the default
    // mask's four-character tail would reach past the `@` into the password
    // whenever the host is shorter than four characters. `mask_db_url` takes
    // its tail from the host alone (#741).
    Rule {
        id: "database-url-credentials",
        provider: "Postgres/MySQL",
        confidence: Confidence::High,
        pattern: r#"\b(?:postgres(?:ql)?|mysql|mariadb)(?:\+[a-z0-9]+)?://[^:/@\s"'<>]+:[^@/\s"'<>${}]+@[A-Za-z0-9.-]+"#,
        accept: Some(real_db_password),
        paired_with: None,
        mask: Some(mask_db_url),
    },
    // gitleaks' `private-key`, which requires 64 characters of body so a bare
    // header in documentation is not a key. `\\` is in the body class because a
    // transcript holds the block JSON-escaped, newlines as `\n`.
    Rule {
        id: "private-key",
        provider: "PEM",
        confidence: Confidence::High,
        pattern: r"-----BEGIN (?:(?:RSA|EC|DSA|OPENSSH|PGP|ENCRYPTED) )?PRIVATE KEY(?: BLOCK)?-----[A-Za-z0-9+/=\s\\:,.-]{64,}?-----END (?:(?:RSA|EC|DSA|OPENSSH|PGP|ENCRYPTED) )?PRIVATE KEY(?: BLOCK)?-----",
        accept: None,
        paired_with: None,
        mask: None,
    },
    // `eyJ` is base64 for `{"`, so this is a JSON header, a JSON payload and a
    // signature. Medium because a JWT is often not a secret at all — an ID
    // token, a public example — which is exactly what the tier is for.
    Rule {
        id: "jwt",
        provider: "JWT",
        confidence: Confidence::Medium,
        pattern: r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
        accept: None,
        paired_with: None,
        mask: None,
    },
];

/// Every rule with its compiled pattern, built once per process.
pub fn compiled() -> &'static [(Rule, Regex)] {
    static TABLE: OnceLock<Vec<(Rule, Regex)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        RULES
            .iter()
            .map(|r| {
                let re = Regex::new(r.pattern)
                    .unwrap_or_else(|e| panic!("rule {} does not compile: {e}", r.id));
                (*r, re)
            })
            .collect()
    })
}

/// The custom mask of the rule `id`, if it has one. An id outside the table
/// has none.
pub(super) fn mask_of(id: &str) -> Option<fn(&str) -> String> {
    RULES.iter().find(|r| r.id == id).and_then(|r| r.mask)
}

fn aws_secret_shape(s: &str) -> bool {
    s.len() == 40
        && s.bytes().any(|b| b.is_ascii_uppercase())
        && s.bytes().any(|b| b.is_ascii_lowercase())
        && s.bytes().any(|b| b.is_ascii_digit())
}

/// Refuses templated passwords and the words examples use in place of one.
fn real_db_password(url: &str) -> bool {
    let Some(rest) = url.split_once("://").map(|(_, r)| r) else {
        return false;
    };
    let Some((userinfo, _host)) = rest.rsplit_once('@') else {
        return false;
    };
    let Some((_user, password)) = userinfo.split_once(':') else {
        return false;
    };
    // A `%` is credited only as a percent-escape (`%40`), so a format string
    // (`%s`, `%v`, `%(pw)s`) or a batch variable (`%DB_PASSWORD%`) is not.
    let escapes_only = password
        .split('%')
        .skip(1)
        .all(|s| s.len() >= 2 && s.as_bytes()[..2].iter().all(u8::is_ascii_hexdigit));
    if !escapes_only {
        return false;
    }
    const PLACEHOLDERS: &[&str] = &[
        "password",
        "passwd",
        "pass",
        "pwd",
        "secret",
        "changeme",
        "example",
        "xxx",
        "xxxx",
        "xxxxxxxx",
        "***",
        "****",
        "********",
        "user",
        "username",
        "your_password",
        "your-password",
        "yourpassword",
        "mysecretpassword",
    ];
    !PLACEHOLDERS
        .iter()
        .any(|p| password.eq_ignore_ascii_case(p))
}

/// A database URL's display form: the first four characters, which always lie
/// inside the scheme, the fixed run, and the last four characters **of the
/// host** — fewer when the host is shorter, so the tail never reaches back
/// past the `@`. The password class excludes `@`, so the last one is the
/// delimiter, the same split [`real_db_password`] makes. A text with no `@`
/// is not this rule's match and shows nothing.
fn mask_db_url(url: &str) -> String {
    let Some((_, host)) = url.rsplit_once('@') else {
        return store::MASK_RUN.to_string();
    };
    let head: String = url.chars().take(4).collect();
    let host: Vec<char> = host.chars().collect();
    let tail: String = host[host.len().saturating_sub(4)..].iter().collect();
    format!("{head}{}{tail}", store::MASK_RUN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_compiles_and_ids_are_unique() {
        let table = compiled();
        assert_eq!(table.len(), RULES.len());
        let mut ids: Vec<_> = table.iter().map(|(r, _)| r.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), RULES.len(), "duplicate rule id");
    }

    #[test]
    fn jwt_is_the_only_medium_rule() {
        let medium: Vec<_> = RULES
            .iter()
            .filter(|r| r.confidence == Confidence::Medium)
            .map(|r| r.id)
            .collect();
        assert_eq!(medium, ["jwt"]);
    }

    #[test]
    fn the_database_url_is_the_only_rule_with_its_own_mask() {
        let custom: Vec<_> = RULES
            .iter()
            .filter(|r| r.mask.is_some())
            .map(|r| r.id)
            .collect();
        assert_eq!(custom, ["database-url-credentials"]);
        assert!(mask_of("database-url-credentials").is_some());
        assert!(mask_of("github-pat").is_none());
        assert!(mask_of("no-such-rule").is_none());
    }

    #[test]
    fn a_paired_rule_names_a_rule_that_exists() {
        for r in RULES {
            if let Some(p) = r.paired_with {
                assert!(RULES.iter().any(|o| o.id == p), "{} pairs with {p}", r.id);
            }
        }
    }
}
