//! `text -> Vec<Finding>`: every rule in [`rules::compiled`] over one input.
//!
//! Pure — no I/O and no state — so the worker (#603) and, later, a Claude Code
//! hook can call it on whatever text they hold. Three passes:
//!
//! 1. every rule collects its non-overlapping matches, filtered by the rule's
//!    `accept` check;
//! 2. a paired rule's matches are kept only within [`rules::PAIR_WINDOW`] bytes
//!    of a match of its partner (the AWS secret key beside its key id);
//! 3. a finding lying wholly inside another rule's finding is dropped, so one
//!    secret is one finding — a service-account file's `private_key` is
//!    reported as that, not also as a bare PEM block.
//!
//! Findings come back ordered by `start`, then by rule table order.

use super::rules::{self, Confidence, PAIR_WINDOW};

/// One match. Byte offsets into the scanned text — **never the text itself**
/// (see the module header of `security_scan`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finding {
    pub rule_id: &'static str,
    pub confidence: Confidence,
    pub start: usize,
    pub end: usize,
}

pub fn scan(text: &str) -> Vec<Finding> {
    let table = rules::compiled();
    let mut found: Vec<(usize, Finding)> = Vec::new();

    for (order, (rule, re)) in table.iter().enumerate() {
        for m in re.find_iter(text) {
            if rule.accept.is_some_and(|accept| !accept(m.as_str())) {
                continue;
            }
            found.push((
                order,
                Finding {
                    rule_id: rule.id,
                    confidence: rule.confidence,
                    start: m.start(),
                    end: m.end(),
                },
            ));
        }
    }

    let found = keep_paired(table, found);
    let mut found = drop_contained(found);
    found.sort_by_key(|(order, f)| (f.start, *order));
    found.into_iter().map(|(_, f)| f).collect()
}

/// Pass 2. One rule's matches never overlap (`find_iter`), so each rule's
/// spans sorted by start are sorted by end too, and "is any partner within the
/// window" is one binary search — the pass stays `O(F log F)` in findings.
fn keep_paired(
    table: &[(rules::Rule, regex::Regex)],
    found: Vec<(usize, Finding)>,
) -> Vec<(usize, Finding)> {
    // Spans per rule, in `find_iter` order, i.e. ascending.
    let mut spans: Vec<Vec<(usize, usize)>> = vec![Vec::new(); table.len()];
    for (order, f) in &found {
        spans[*order].push((f.start, f.end));
    }
    let partner: Vec<Option<usize>> = table
        .iter()
        .map(|(r, _)| {
            r.paired_with
                .and_then(|id| table.iter().position(|(p, _)| p.id == id))
        })
        .collect();

    found
        .into_iter()
        .filter(|(order, f)| {
            let Some(p) = partner[*order] else {
                return true;
            };
            let near = &spans[p];
            // The first partner that does not end before the window opens.
            let i = near.partition_point(|&(_, end)| end + PAIR_WINDOW < f.start);
            near.get(i)
                .is_some_and(|&(start, _)| start <= f.end + PAIR_WINDOW)
        })
        .collect()
}

/// Pass 3: drop a finding lying wholly inside another, differently-spanned
/// one. Sorted by start, then widest first, a finding is contained exactly
/// when something before it reaches past its end — or reaches its end from an
/// earlier start. Two rules matching the identical span are both kept.
fn drop_contained(mut found: Vec<(usize, Finding)>) -> Vec<(usize, Finding)> {
    found.sort_by_key(|(order, f)| (f.start, std::cmp::Reverse(f.end), *order));
    // The furthest end seen, and the start of the first finding to reach it.
    let mut reach: Option<(usize, usize)> = None;
    found.retain(|(_, f)| {
        let contained =
            reach.is_some_and(|(end, start)| end > f.end || (end == f.end && start < f.start));
        if reach.is_none_or(|(end, _)| f.end > end) {
            reach = Some((f.end, f.start));
        }
        !contained
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every fixture is synthetic, and every one is assembled at run time from
    // pieces, so no string literal in this file has a credential's shape — a
    // push-protection scanner reading the source must not mistake a test for a
    // leak, and neither must this checker scanning a transcript that read it.

    fn cat(parts: &[&str]) -> String {
        parts.concat()
    }

    /// `n` characters cycling through `alphabet`.
    fn body(alphabet: &str, n: usize) -> String {
        alphabet.chars().cycle().take(n).collect()
    }

    const ALNUM: &str = "aB3dE5gH7jK9mN1pQ2rS4tU6vW8xY0z";

    fn ids(text: &str) -> Vec<&'static str> {
        scan(text).into_iter().map(|f| f.rule_id).collect()
    }

    /// Asserts exactly one finding, of `rule`, covering exactly `secret`.
    fn assert_finds(rule: &str, secret: &str) {
        let text = cat(&["output: ", secret, " (done)\n"]);
        let found = scan(&text);
        assert_eq!(found.iter().map(|f| f.rule_id).collect::<Vec<_>>(), [rule]);
        let f = found[0];
        assert_eq!(&text[f.start..f.end], secret);
    }

    #[test]
    fn aws_access_key_id() {
        assert_finds("aws-access-key-id", &cat(&["AK", "IA", "Q7ZX3MPLR2VN6TWB"]));
    }

    #[test]
    fn aws_secret_key_is_credited_beside_its_key_id() {
        let id = cat(&["AK", "IA", "Q7ZX3MPLR2VN6TWB"]);
        let secret = body("wJalrXUtnFEMI/K7MDENG+bPxRfiCY9z", 40);
        let text = cat(&[
            "[default]\naws_access_key_id = ",
            &id,
            "\naws_secret_access_key = ",
            &secret,
            "\n",
        ]);
        let found = scan(&text);
        assert_eq!(
            found.iter().map(|f| f.rule_id).collect::<Vec<_>>(),
            ["aws-access-key-id", "aws-secret-access-key"]
        );
        assert_eq!(&text[found[1].start..found[1].end], secret);
    }

    #[test]
    fn aws_secret_key_in_env_and_export_form() {
        let id = cat(&["AK", "IA", "Q7ZX3MPLR2VN6TWB"]);
        let secret = body("wJalrXUtnFEMI/K7MDENG+bPxRfiCY9z", 40);
        for (a, b) in [
            ("AWS_ACCESS_KEY_ID=", "\nAWS_SECRET_ACCESS_KEY="),
            (
                "export AWS_ACCESS_KEY_ID=",
                "\nexport AWS_SECRET_ACCESS_KEY=",
            ),
            ("{\"AccessKeyId\":\"", "\",\"SecretAccessKey\":\""),
        ] {
            let text = cat(&[a, &id, b, &secret, "\"\n"]);
            let found = scan(&text);
            assert_eq!(
                found.iter().map(|f| f.rule_id).collect::<Vec<_>>(),
                ["aws-access-key-id", "aws-secret-access-key"],
                "form {a:?}"
            );
            assert_eq!(&text[found[1].start..found[1].end], secret);
        }
    }

    #[test]
    fn aws_secret_one_space_after_another_candidate_is_still_found() {
        let id = cat(&["AK", "IA", "Q7ZX3MPLR2VN6TWB"]);
        let sha1 = body("3f786850e387550fdab836ed7e6dc881de23001b", 40);
        let secret = body("wJalrXUtnFEMI/K7MDENG+bPxRfiCY9z", 40);
        assert_eq!(
            ids(&cat(&[&id, " ", &sha1, " ", &secret])),
            ["aws-access-key-id", "aws-secret-access-key"]
        );
    }

    #[test]
    fn aws_secret_inside_a_longer_base64_run_is_not_credited() {
        let id = cat(&["AK", "IA", "Q7ZX3MPLR2VN6TWB"]);
        let long = body("wJalrXUtnFEMI/K7MDENG+bPxRfiCY9z", 41);
        let padded = cat(&[&body("wJalrXUtnFEMI/K7MDENG+bPxRfiCY9z", 40), "=="]);
        assert_eq!(ids(&cat(&[&id, " ", &long])), ["aws-access-key-id"]);
        assert_eq!(ids(&cat(&[&id, " ", &padded])), ["aws-access-key-id"]);
    }

    #[test]
    fn aws_secret_shape_alone_or_far_away_is_not_credited() {
        let secret = body("wJalrXUtnFEMI/K7MDENG+bPxRfiCY9z", 40);
        assert!(ids(&cat(&["secret = ", &secret])).is_empty());

        let id = cat(&["AK", "IA", "Q7ZX3MPLR2VN6TWB"]);
        let far = cat(&[&id, &" ".repeat(PAIR_WINDOW + 1), &secret]);
        assert_eq!(ids(&far), ["aws-access-key-id"]);
    }

    #[test]
    fn aws_secret_needs_mixed_case_and_digits_even_beside_a_key_id() {
        // A SHA-1 next to a key id: 40 characters, but hex.
        let id = cat(&["AK", "IA", "Q7ZX3MPLR2VN6TWB"]);
        let sha1 = body("3f786850e387550fdab836ed7e6dc881de23001b", 40);
        assert_eq!(ids(&cat(&[&id, " ", &sha1])), ["aws-access-key-id"]);
    }

    fn pem_body() -> String {
        (0..6)
            .map(|_| body("MIIEvQIBADANBgkqhkiG9w0BAQEFAASC", 64))
            .collect::<Vec<_>>()
            .join("\\n")
    }

    #[test]
    fn gcp_service_account_private_key() {
        let begin = cat(&["-----BEGIN ", "PRIVATE KEY-----"]);
        let end = cat(&["-----END ", "PRIVATE KEY-----"]);
        let member = cat(&[
            "\"private_key\": \"",
            &begin,
            "\\n",
            &pem_body(),
            "\\n",
            &end,
            "\\n\"",
        ]);
        let text = cat(&[
            "{\"type\": \"service_account\", \"project_id\": \"demo\", ",
            &member,
            ", \"client_email\": \"x@demo.iam.gserviceaccount.com\"}",
        ]);
        let found = scan(&text);
        // One finding, not also a `private-key` one inside it.
        assert_eq!(
            found.iter().map(|f| f.rule_id).collect::<Vec<_>>(),
            ["gcp-service-account-key"]
        );
        assert_eq!(&text[found[0].start..found[0].end], member);
    }

    #[test]
    fn azure_connection_string() {
        let key = cat(&[&body("Zm9vYmFyYmF6cXV4K3Nsb3Q/", 86), "=="]);
        let text = cat(&[
            "DefaultEndpointsProtocol=https;AccountName=demo;AccountKey=",
            &key,
            ";EndpointSuffix=core.windows.net",
        ]);
        let found = scan(&text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rule_id, "azure-connection-string");
        assert_eq!(
            &text[found[0].start..found[0].end],
            cat(&["AccountKey=", &key])
        );

        let sb = cat(&[
            "Endpoint=sb://demo.servicebus.windows.net/;SharedAccessKeyName=Root;SharedAccessKey=",
            &body("Zm9vYmFyYmF6cXV4K3Nsb3Q", 43),
            "=",
        ]);
        assert_eq!(ids(&sb), ["azure-connection-string"]);
    }

    #[test]
    fn github_tokens() {
        assert_finds("github-pat", &cat(&["gh", "p_", &body(ALNUM, 36)]));
        assert_finds("github-oauth", &cat(&["gh", "o_", &body(ALNUM, 36)]));
        assert_finds(
            "github-fine-grained-pat",
            &cat(&["github", "_pat_", &body("11ABCDE0Y0_aB3dE5gH7jK9", 82)]),
        );
    }

    #[test]
    fn slack_tokens() {
        for kind in ["b", "a", "p", "r", "s"] {
            assert_finds(
                "slack-token",
                &cat(&[
                    "xo",
                    "x",
                    kind,
                    "-",
                    "1234567890",
                    "-",
                    "4815162342",
                    "-",
                    &body(ALNUM, 24),
                ]),
            );
        }
    }

    #[test]
    fn stripe_live_keys() {
        assert_finds("stripe-live-key", &cat(&["sk", "_live_", &body(ALNUM, 24)]));
        assert_finds("stripe-live-key", &cat(&["rk", "_live_", &body(ALNUM, 99)]));
    }

    #[test]
    fn openai_keys() {
        assert_finds("openai-api-key", &cat(&["sk", "-", &body(ALNUM, 48)]));
        assert_finds(
            "openai-api-key",
            &cat(&["sk", "-proj-", &body("aB3_dE5-gH7jK9", 120)]),
        );
    }

    #[test]
    fn anthropic_key_is_not_also_an_openai_key() {
        assert_finds(
            "anthropic-api-key",
            &cat(&["sk", "-ant-", "api03-", &body("aB3_dE5-gH7jK9", 93), "AA"]),
        );
    }

    #[test]
    fn npm_token() {
        assert_finds("npm-access-token", &cat(&["np", "m_", &body(ALNUM, 36)]));
    }

    #[test]
    fn database_urls_with_credentials() {
        for scheme in [
            "postgres",
            "postgresql",
            "postgresql+psycopg2",
            "mysql",
            "mariadb",
        ] {
            assert_finds(
                "database-url-credentials",
                &cat(&[
                    scheme,
                    "://app:",
                    "Tr0ub4dor-3xq",
                    "@db.internal.example.com",
                ]),
            );
        }
        // `@`, `/` and `:` in a password can only travel percent-encoded.
        assert_finds(
            "database-url-credentials",
            &cat(&["postgres://app:", "p%40ssW0rd9", "@db.example.com"]),
        );
    }

    #[test]
    fn pem_private_keys() {
        for kind in ["", "RSA ", "EC ", "OPENSSH ", "ENCRYPTED "] {
            let key = cat(&[
                "-----BEGIN ",
                kind,
                "PRIVATE KEY-----\n",
                &pem_body().replace("\\n", "\n"),
                "\n-----END ",
                kind,
                "PRIVATE KEY-----",
            ]);
            assert_finds("private-key", &key);
        }
        let pgp = cat(&[
            "-----BEGIN PGP ",
            "PRIVATE KEY BLOCK-----\n\n",
            &pem_body().replace("\\n", "\n"),
            "\n-----END PGP PRIVATE KEY BLOCK-----",
        ]);
        assert_finds("private-key", &pgp);
    }

    #[test]
    fn jwt_is_medium() {
        let jwt = cat(&[
            "ey",
            "JhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.",
            "ey",
            "JzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkRlbW8ifQ.",
            "SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
        ]);
        assert_finds("jwt", &jwt);
        assert_eq!(scan(&jwt)[0].confidence, Confidence::Medium);
        assert_eq!(
            scan(&cat(&["gh", "p_", &body(ALNUM, 36)]))[0].confidence,
            Confidence::High
        );
    }

    #[test]
    fn findings_are_ordered_by_offset() {
        let text = cat(&[
            "np",
            "m_",
            &body(ALNUM, 36),
            " then ",
            "gh",
            "p_",
            &body(ALNUM, 36),
            " then ",
            "np",
            "m_",
            &body(ALNUM, 36),
        ]);
        let found = scan(&text);
        assert_eq!(
            found.iter().map(|f| f.rule_id).collect::<Vec<_>>(),
            ["npm-access-token", "github-pat", "npm-access-token"]
        );
        assert!(found.windows(2).all(|w| w[0].start < w[1].start));
    }

    // ── the false-positive bar ────────────────────────────────────────────

    #[test]
    fn known_false_positive_shapes_find_nothing() {
        let shapes = [
            // UUID v4
            "session 3b241101-e2bb-4255-8caf-4136c566a962 resumed".to_string(),
            // SHA-256 and SHA-1 hex digests, a git log line
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".to_string(),
            "commit 3f786850e387550fdab836ed7e6dc881de23001b\nAuthor: x".to_string(),
            // A base64 blob (an embedded PNG)
            cat(&["data:image/png;base64,", &body("iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB", 4096)]),
            // Minified JS
            "!function(e,t){\"object\"==typeof exports&&\"undefined\"!=typeof module?module.exports=t():e.sk=t()}(this,function(){var n=\"task-runner-pool-size\";return{skip:n}});".to_string(),
            // Placeholders from docs and config templates
            cat(&["SLACK_BOT_TOKEN=xo", "xb-your-bot-token-here"]),
            cat(&["DATABASE_URL=postgres://", "user:password@localhost:5432/app"]),
            cat(&["DATABASE_URL=postgres://", "app:${DB_PASSWORD}@db:5432/app"]),
            cat(&["mysql://", "root:<password>@127.0.0.1"]),
            cat(&["DSN = 'postgresql://", "app:%(password)s@db/app' % cfg"]),
            cat(&["\"postgresql://", "app:%s@localhost:5432/app\" % pw"]),
            cat(&["fmt.Sprintf(\"postgres://", "app:%v@db:5432/app\", pw)"]),
            cat(&["set DSN=postgres://", "app:%DB_PASSWORD%@db/app"]),
            cat(&["postgres://", "postgres:mysecretpassword@localhost/postgres"]),
            cat(&["mysql://", "app:your_password@localhost/app"]),
            // A PEM header with no key body, as in documentation
            cat(&["-----BEGIN RSA ", "PRIVATE KEY-----\n...\n-----END RSA PRIVATE KEY-----"]),
            // A public key and a certificate are not secrets
            cat(&["-----BEGIN ", "PUBLIC KEY-----\n", &body("MIIBIjANBgkqhkiG9w0BAQEFAAOC", 300), "\n-----END PUBLIC KEY-----"]),
            // Prefix-alikes that are too short or embedded in a word
            "the task-runner and disk-usage-monitor-daemon tools".to_string(),
            cat(&["mask_live_", &body(ALNUM, 30)]),
        ];
        // Failure messages name the shape by index, never the text: a scanner
        // reading a failing log should not see a credential-shaped string.
        for (i, s) in shapes.iter().enumerate() {
            assert!(
                scan(s).is_empty(),
                "false positive {:?} in shape #{i}",
                ids(s)
            );
        }
    }

    #[test]
    fn a_large_adversarial_input_finds_nothing_and_finishes() {
        // Prefixes with no body, headers with no end: every rule starts a match
        // thousands of times and completes none. The `regex` crate is linear
        // time, so this is a correctness check on the patterns' bounds rather
        // than a benchmark.
        let unit = cat(&[
            "-----BEGIN RSA ",
            "PRIVATE KEY----- ey",
            "J",
            "abc.ey",
            "J. postgres://a: xo",
            "xb-1 sk",
            "-ant- AK",
            "IA ",
            &body("ABCDEFGHIJKLMNOPQRSTUVWXYZ012345", 39),
            " ",
        ]);
        let text = unit.repeat(2 * 1024 * 1024 / unit.len());
        assert!(scan(&text).is_empty());
    }

    #[test]
    fn many_findings_stay_linearithmic() {
        // The post-passes are per finding, so this is the input that would
        // expose a quadratic one: tens of thousands of JWTs in ~3 MB, each
        // beside an AWS-secret-shaped run that has to look for a partner.
        let jwt = cat(&[
            "ey",
            "JhbGciOiJIUzI1NiJ9.",
            "ey",
            "JzdWIiOiIxMjM0In0.",
            "SflKxwRJSMeKKF2QT4fw",
        ]);
        let unit = cat(&[
            &jwt,
            " ",
            &body("wJalrXUtnFEMI/K7MDENG+bPxRfiCY9z", 40),
            "\n",
        ]);
        let n = 3 * 1024 * 1024 / unit.len();
        let text = unit.repeat(n);
        let started = std::time::Instant::now();
        let found = scan(&text);
        assert_eq!(found.len(), n);
        assert!(found.iter().all(|f| f.rule_id == "jwt"));
        // Generous for an unoptimised build on a loaded runner; the quadratic
        // passes this replaced took several seconds on this input.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }
}
