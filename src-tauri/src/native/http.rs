//! The one place an outbound HTTP client is built.
//!
//! # Why this module exists at all
//!
//! Go's `net/http` sets `User-Agent: Go-http-client/1.1` on every request
//! without being asked; `reqwest` sets **nothing** unless asked, and the port
//! never asked. The header was lost silently, because it was written down on
//! neither side — there is no response byte, no stored value and no parity
//! vector that records it (`parity/google_vectors.json` says so explicitly:
//! `X-Goog-Api-Client` and `User-Agent` are deliberately not recorded).
//!
//! GitHub *requires* the header and answers **403** without it, with a message
//! that names the credential rather than the header:
//!
//! ```text
//! Request forbidden by administrative rules. Please make sure your request has
//! a User-Agent header
//! ```
//!
//! So `validate_pat` reported `validation error for
//! "credentials.personal_access_token"` for a perfectly good token, and since
//! it shares `http_client()` with the tool path, all twenty GitHub tools 403'd
//! the same way. The other five integrations were merely anonymous — they work
//! today only because those APIs do not enforce a rule GitHub does.
//!
//! # The rule
//!
//! **Every `reqwest` client the app builds comes from [`client_builder`].**
//! Not "every client that talks to GitHub" — the point of a single factory is
//! that the eighth client cannot silently omit the header, which is exactly how
//! the first seven came to. `no_client_is_built_outside_this_module` is the
//! guard, and it reads the crate's own sources rather than trusting the rule to
//! be remembered.
//!
//! What the factory does **not** do is unify anything else. Timeouts differ per
//! integration on purpose (15s GitHub/Jira, 30s Confluence, 60s
//! Slack/Telegram/Google), GitHub keeps its second no-redirect client, and the
//! gateway catalog keeps `redirect::Policy::none()` because its credential
//! rides a custom header `reqwest` does not strip across a redirect. Each call
//! site adds its own; this adds one header and nothing else.

/// `Agento/<crate version> (+https://myagento.app)`.
///
/// RFC 9110 `product/version` with an informational comment, and nothing else:
/// no OS, no arch, no webview version. Those are fingerprinting surface on a
/// desktop application and nothing downstream needs them — what a provider's
/// abuse heuristic rewards is a stable product token with a contact URL.
///
/// **The version is [`env!`]`("CARGO_PKG_VERSION")`, not
/// [`crate::native::version::VERSION`].** That one is
/// `option_env!("AGENTO_BUILD_VERSION")` and answers `dev` in every unstamped
/// build — deliberately and correctly for the About screen, and a shipped
/// failure mode besides (every release through `desktop-v0.1.1` reported `dev`
/// because nothing set the variable). `src-tauri/Cargo.toml`'s version is one
/// of the five places the release commit bumps and CI enforces on every PR, so
/// it is right in a shipped installer *and* right in a local build.
///
/// `concat!` keeps it a `&'static str`: no allocation, no lazy init, and the
/// version is fixed at compile time rather than read back from anywhere.
pub const USER_AGENT: &str = concat!(
    "Agento/",
    env!("CARGO_PKG_VERSION"),
    " (+https://myagento.app)"
);

/// A [`reqwest::ClientBuilder`] with [`USER_AGENT`] already applied.
///
/// Note `.user_agent()` sets a **default** header, so an explicit per-request
/// `.header(USER_AGENT, …)` would still win. Nothing in the tree sets one, and
/// nothing should — a per-call override would put this module back to being
/// advisory.
pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().user_agent(USER_AGENT)
}

/// A loopback recorder, for the per-integration tests that assert the header is
/// on the wire rather than on the builder.
///
/// Reading `.user_agent()` back off a `ClientBuilder` is not possible and
/// asserting that a call site *calls* [`client_builder`] would only restate the
/// guard below. So each client's own module drives one real request through its
/// own constructor and reads the header the server received.
#[cfg(test)]
pub(crate) mod testing {
    /// Send one `GET` through `client` and answer with the `User-Agent` the
    /// server saw — the empty string when none arrived.
    pub(crate) async fn user_agent_seen_by_a_server(client: &reqwest::Client) -> String {
        use std::sync::{Arc, Mutex};

        let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let recorder = seen.clone();

        let app = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
            let recorder = recorder.clone();
            async move {
                *recorder.lock().expect("recorder lock") = Some(
                    headers
                        .get(axum::http::header::USER_AGENT)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string(),
                );
                "ok"
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the recorder");
        let url = format!("http://{}/", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        client
            .get(&url)
            .send()
            .await
            .expect("the recorder answered")
            .bytes()
            .await
            .expect("a body");

        let seen = seen.lock().expect("recorder lock").clone();
        seen.expect("the recorder saw no request at all")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The version in the header is the crate's, checked against the crate's
    /// own version rather than against a literal — a release bump moves
    /// `CARGO_PKG_VERSION` and this assertion moves with it, where a spelled-out
    /// `1.2.0` would sit there being wrong for a whole release.
    #[test]
    fn the_user_agent_carries_the_crate_version() {
        assert_eq!(
            USER_AGENT,
            format!(
                "Agento/{} (+https://myagento.app)",
                env!("CARGO_PKG_VERSION")
            ),
        );
        assert!(
            !env!("CARGO_PKG_VERSION").is_empty(),
            "an empty crate version would make the assertion above vacuous"
        );
    }

    /// A header value `reqwest` refuses is a client that fails to build, which
    /// is `Option::None` at seven call sites and no outbound call anywhere.
    #[test]
    fn the_user_agent_is_a_legal_header_value() {
        let value = reqwest::header::HeaderValue::from_str(USER_AGENT)
            .expect("the User-Agent is not a legal header value");
        assert_eq!(value.to_str().expect("visible ASCII"), USER_AGENT);
    }

    /// The shape a provider's heuristic reads: `product/version` plus a comment
    /// carrying a contact URL, and no third token.
    #[test]
    fn the_user_agent_has_the_documented_shape() {
        let (product, comment) = USER_AGENT
            .split_once(' ')
            .expect("a product token and a comment");
        let version = product
            .strip_prefix("Agento/")
            .expect("the product token is `Agento/<version>`");
        assert_eq!(comment, "(+https://myagento.app)");
        assert_eq!(version.split('.').count(), 3, "a three-part version");
        assert!(
            version
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())),
            "every version part is numeric: {version:?}"
        );
    }

    /// Every outbound client is built here, and this is what keeps it true.
    ///
    /// The seven that existed before this module were each written
    /// independently and each omitted the header; a rule that lives only in a
    /// doc comment would be re-omitted by the eighth. So the crate's own
    /// sources are read and any `reqwest::Client::builder()` or
    /// `reqwest::Client::new()` outside this module fails the build.
    ///
    /// Test code is exempt two ways, and neither is a filename convention:
    /// a `#[cfg(test)]` region within a file (`claude/mcp.rs`,
    /// `claude/tool.rs`, `native/tools/mod.rs`), and a whole file whose parent
    /// declares it `#[cfg(test)] mod …;` (the six `tests_vectors.rs`). The
    /// second set is **derived from the declarations**, so naming a production
    /// file `tests_something.rs` buys it no exemption. Those all drive loopback
    /// fakes, where the header is the thing being asserted rather than
    /// something to send.
    ///
    /// Comment lines are skipped, or this doc comment would report itself.
    #[test]
    fn no_client_is_built_outside_this_module() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let sources = rust_sources(&root);
        let test_only = test_only_files(&root, &sources);
        let mut offenders = Vec::new();

        for path in &sources {
            let relative = path
                .strip_prefix(&root)
                .expect("under src/")
                .to_string_lossy()
                .replace('\\', "/");
            if relative == "native/http.rs" || test_only.contains(&relative) {
                continue;
            }
            let source = std::fs::read_to_string(path).expect("a source file");
            for (number, line) in source.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if !line.contains("reqwest::Client::builder()")
                    && !line.contains("reqwest::Client::new()")
                {
                    continue;
                }
                if in_test_code(&source, number) {
                    continue;
                }
                offenders.push(format!("{}:{}", relative, number + 1));
            }
        }

        assert!(
            offenders.is_empty(),
            "these build a reqwest client outside native::http, so it sends no User-Agent \
             and GitHub answers 403: {offenders:?}. Use `crate::native::http::client_builder()`."
        );
    }

    /// The files some other file declares as `#[cfg(test)] mod <name>;` — the
    /// whole file compiles only under `cargo test`, so nothing in it ever
    /// reaches a user's machine.
    fn test_only_files(
        root: &std::path::Path,
        sources: &[std::path::PathBuf],
    ) -> std::collections::BTreeSet<String> {
        let mut found = std::collections::BTreeSet::new();
        for path in sources {
            let source = std::fs::read_to_string(path).expect("a source file");
            let dir = path.parent().expect("a parent dir").to_path_buf();
            let mut lines = source.lines().peekable();
            while let Some(line) = lines.next() {
                if line.trim() != "#[cfg(test)]" {
                    continue;
                }
                let Some(next) = lines.peek() else { continue };
                let Some(name) = next
                    .trim()
                    .strip_prefix("mod ")
                    .and_then(|rest| rest.strip_suffix(';'))
                else {
                    continue;
                };
                for candidate in [
                    dir.join(format!("{name}.rs")),
                    dir.join(name).join("mod.rs"),
                ] {
                    if let Ok(relative) = candidate.strip_prefix(root) {
                        found.insert(relative.to_string_lossy().replace('\\', "/"));
                    }
                }
            }
        }
        found
    }

    /// Every `.rs` file under `src-tauri/src`.
    fn rust_sources(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("readable source dir") {
                let path = entry.expect("a dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    found.push(path);
                }
            }
        }
        found.sort();
        found
    }

    /// Is line `number` (0-based) inside a `#[cfg(test)]` item?
    ///
    /// **An attribute covers exactly the one item after it**, and getting that
    /// wrong is a hole rather than a false alarm — which is why it is spelled
    /// out rather than approximated by "count braces from the last
    /// `#[cfg(test)]`". Five of the client files open with
    /// `#[cfg(test)]\nuse std::sync::RwLock;`, a *brace-less* item; a walk that
    /// stayed "inside" until the next `}` then treated everything between that
    /// `use` and the end of the next block as test code, and a production
    /// client written there was exempted silently. Verified by putting one
    /// there and watching the guard pass.
    ///
    /// So: the line after the attribute is the item. Braces balanced on it →
    /// the item ended there. Otherwise track depth to zero.
    fn in_test_code(source: &str, number: usize) -> bool {
        let mut pending = false;
        let mut inside = false;
        let mut depth = 0i32;

        for (index, line) in source.lines().enumerate() {
            let delta = line.matches('{').count() as i32 - line.matches('}').count() as i32;

            if inside {
                depth += delta;
                if index == number {
                    return true;
                }
                if depth <= 0 {
                    inside = false;
                }
                continue;
            }
            if pending {
                pending = false;
                if delta > 0 {
                    inside = true;
                    depth = delta;
                }
                // A stacked attribute (`#[cfg(test)]` then `#[allow(…)]`) is
                // not the item, so keep waiting for one.
                if line.trim_start().starts_with("#[") {
                    pending = true;
                    inside = false;
                }
                if index == number {
                    return true;
                }
                continue;
            }
            if line.trim_start().starts_with("#[cfg(test)]") {
                pending = true;
                if index == number {
                    return true;
                }
                continue;
            }
            if index == number {
                return false;
            }
        }
        false
    }

    /// A `#[cfg(test)]` on a brace-less item covers that item and stops.
    ///
    /// This is the shape five client files actually open with, and the version
    /// of `in_test_code` this replaced answered `true` for every line after it
    /// until the next `}` — so a production client written in that window was
    /// exempted from the guard with nothing to say so. Reverting the fix fails
    /// here.
    #[test]
    fn a_cfg_test_use_does_not_exempt_the_code_after_it() {
        let source = "#[cfg(test)]\nuse std::sync::RwLock;\n\
                      fn leaked() {\n    reqwest::Client::new();\n}\n";
        assert!(
            in_test_code(source, 1),
            "line 2 is the item the attribute covers"
        );
        assert!(
            !in_test_code(source, 3),
            "line 4 is production code: an attribute covers one item, not a region"
        );
    }

    /// The guard's own reading of test code, pinned — a `in_test_code` that
    /// answered `true` everywhere would make the guard above vacuous while
    /// still passing.
    #[test]
    fn the_guard_tells_test_code_from_the_rest() {
        let source = "fn a() {\n    reqwest::Client::new();\n}\n\
                      #[cfg(test)]\nmod tests {\n    fn b() {\n        \
                      reqwest::Client::new();\n    }\n}\n\
                      fn c() {\n    reqwest::Client::new();\n}\n";
        assert!(!in_test_code(source, 1), "line 2 is production code");
        assert!(in_test_code(source, 6), "line 7 is inside #[cfg(test)]");
        assert!(
            !in_test_code(source, 10),
            "line 11 is after the test module"
        );
    }

    /// The exemption set is the other way the guard could go vacuous: one that
    /// exempted every file would pass for ever while the header went missing
    /// again. So it is pinned from both sides — the six vector files are in it,
    /// and every file this fix actually touched is not.
    #[test]
    fn the_guards_exemptions_cover_the_vector_files_and_nothing_that_ships() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let exempt = test_only_files(&root, &rust_sources(&root));

        for integration in [
            "github",
            "confluence",
            "jira",
            "slack",
            "telegram",
            "google",
        ] {
            assert!(
                exempt.contains(&format!(
                    "native/integrations/{integration}/tests_vectors.rs"
                )),
                "{integration}'s vector file is not recognised as test-only"
            );
            assert!(
                !exempt.contains(&format!("native/integrations/{integration}/client.rs")),
                "{integration}'s client is exempt from the guard, which makes it useless"
            );
        }
        assert!(!exempt.contains("native/gateway_api/catalog.rs"));
        assert!(!exempt.contains("native/integrations/oauth/flow.rs"));
    }
}
